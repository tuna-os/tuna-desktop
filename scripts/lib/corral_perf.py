"""KubeVirt benchmark admission and real RFB transport (RFC6143).

Neither readiness nor a captured framebuffer qualifies performance. Callers must
retain paired image/source identities and the existing native receipt gates.
"""
import json
import hashlib
from decimal import Decimal
import re
import socket
import struct
import subprocess
import time

GIB = 1 << 30
MAX_PIXELS = 4096 * 4096


def resource_count(value):
    """Parse Kubernetes decimal SI quantities for integer extended resources."""
    match = re.fullmatch(r'(\d+(?:\.\d+)?)([kMGTPE]?)', str(value))
    if not match:
        raise ValueError('invalid extended resource quantity')
    count = Decimal(match[1]) * (1000 ** ('kMGTPE'.index(match[2])+1) if match[2] else 1)
    if count != count.to_integral_value():
        raise ValueError('fractional extended resource quantity')
    return int(count)


def node_name(value):
    if not re.fullmatch(r'[a-z0-9](?:[a-z0-9.-]{0,251}[a-z0-9])?', value):
        raise ValueError('invalid Kubernetes node name')
    return value


def kubectl(context, *args, deadline=None):
    if not context or context.startswith('-'):
        raise ValueError('explicit Kubernetes context required')
    timeout=30 if deadline is None else min(30,deadline-time.monotonic())
    if timeout<=0:raise TimeoutError('Kubernetes acquisition deadline')
    result = subprocess.run(['kubectl', '--context', context, *args],
                            capture_output=True, timeout=timeout, check=True)
    if len(result.stdout) > 8*1024*1024:
        raise ValueError('cluster response exceeds bound')
    return json.loads(result.stdout)


def capacity_receipt(node, stats, *, estimated_peak_bytes):
    """Conservative disk admission; configured kubelet thresholds may be higher."""
    if type(estimated_peak_bytes) is not int or estimated_peak_bytes < 0:
        raise ValueError('nonnegative exact peak byte estimate required')
    reasons = []
    conditions = {x['type']: x['status'] for x in node['status'].get('conditions', [])}
    taints = node.get('spec', {}).get('taints', [])
    if conditions.get('Ready') != 'True':
        reasons.append('node-not-ready')
    if conditions.get('DiskPressure') != 'False':
        reasons.append('disk-pressure-or-unknown')
    if any(t.get('effect') in ('NoSchedule', 'NoExecute') for t in taints):
        reasons.append('node-scheduling-taint')
    allocatable = node['status'].get('allocatable', {})
    for resource in ('kvm', 'tun', 'vhost-net'):
        if resource_count(allocatable.get('devices.kubevirt.io/'+resource, '0')) <= 0:
            reasons.append('missing-'+resource)
    filesystems = {'node': stats.get('node', {}).get('fs')}
    runtime = stats.get('node', {}).get('runtime', {})
    filesystems.update(image=runtime.get('imageFs'), container=runtime.get('containerFs'))
    headroom = {}
    for name, value in filesystems.items():
        if not value or not all(type(value.get(key)) is int for key in ('capacityBytes', 'availableBytes')):
            reasons.append(name+'-filesystem-unavailable')
            continue
        # Default imagefs pressure may start at15%, not nodefs10%. Include1Gi
        # operational reserve and use the minimum across all reported pools.
        headroom[name] = max(0, value['availableBytes'] - (value['capacityBytes']*15+99)//100 - GIB)
    available = min(headroom.values()) if len(headroom) == 3 else 0
    if estimated_peak_bytes <= 0:
        reasons.append('disk-peak-estimate-required')
    elif estimated_peak_bytes > available:
        reasons.append('insufficient-disk-headroom')
    gpu_keys = [key for key, value in allocatable.items()
                if ('gpu' in key.lower() or key.startswith('nvidia.com/')) and resource_count(value) > 0]
    return {'schema': 1, 'node': node['metadata']['name'], 'node_uid': node['metadata']['uid'],
            'ready_for_disk_admission': not reasons, 'reasons': reasons,
            'minimum_available_disk_headroom_bytes': available,
            'filesystem_headroom_bytes': headroom, 'estimated_peak_bytes': estimated_peak_bytes,
            'reserve': {'minimum_percent': 15, 'additional_bytes': GIB,
                        'configured_thresholds_verified': False},
            'allocatable_cpu': allocatable.get('cpu'), 'allocatable_memory': allocatable.get('memory'),
            'gpu': {'status': 'not-measured', 'advertised_devices': gpu_keys,
                    'qualified': False},
            'total_power': {'status': 'unsupported', 'qualified': False},
            'performance_qualified': False, 'whole_session_cpu_qualified': False}


class Rfb:
    """Raw framebuffer + actual key events on an owned virtctl localhost tunnel.

    Supports bounded RFB3.8, TrueColour32, Raw and DesktopSize only. Rejects
    authentication extensions instead of guessing or weakening negotiation.
    """
    def __init__(self, stream):
        self.stream = stream
        self.deadline = time.monotonic()+20
        stream.settimeout(20)
        if self.read(12) != b'RFB 003.008\n':
            raise ValueError('unsupported RFB protocol')
        stream.sendall(b'RFB 003.008\n')
        count = self.read(1)[0]
        if count == 0 or 1 not in self.read(count):
            raise ValueError('expected API-tunneled RFB security type None')
        stream.sendall(b'\x01')
        if struct.unpack('>I', self.read(4))[0] != 0:
            raise ValueError('RFB security result rejected')
        stream.sendall(b'\x01')  # Shared client, scoped owned VM only.
        self.width, self.height = struct.unpack('>HH', self.read(4))
        self.read(16)
        length = struct.unpack('>I', self.read(4))[0]
        if length > 4096:
            raise ValueError('RFB server name exceeds bound')
        self.read(length)
        self.resize(self.width, self.height)
        pixel_format = struct.pack('>BBBBHHHBBB3x', 32, 24, 0, 1, 255, 255, 255, 16, 8, 0)
        stream.sendall(b'\x00\x00\x00\x00'+pixel_format)
        stream.sendall(struct.pack('>BBHii', 2, 0, 2, 0, -223))

    def read(self, count):
        if not 0 <= count <= MAX_PIXELS*4:
            raise ValueError('RFB read exceeds bound')
        data = bytearray()
        while len(data) < count:
            remaining = self.deadline-time.monotonic()
            if remaining <= 0:
                raise TimeoutError("RFB acquisition deadline expired")
            self.stream.settimeout(remaining)
            chunk = self.stream.recv(min(65536, count-len(data)))
            if not chunk:
                raise EOFError('RFB connection closed')
            data.extend(chunk)
        return bytes(data)

    def resize(self, width, height):
        if not (0 < width <= 4096 and 0 < height <= 4096 and width*height <= MAX_PIXELS):
            raise ValueError('invalid RFB dimensions')
        self.width, self.height = width, height
        self.pixels = bytearray(width*height*4)

    def snapshot(self):
        self.stream.sendall(struct.pack('>BBHHHH', 3, 0, 0, 0, self.width, self.height))
        deadline = time.monotonic()+20
        self.deadline = deadline
        coverage = [bytearray(self.width) for _ in range(self.height)]
        while time.monotonic() < deadline:
            message = self.read(1)[0]
            if message == 2:  # Bell does not complete a framebuffer request.
                continue
            if message == 3:  # Clipboard is bounded and discarded.
                self.read(3)
                length = struct.unpack('>I', self.read(4))[0]
                if length > 1048576:
                    raise ValueError('RFB clipboard exceeds bound')
                self.read(length)
                continue
            if message != 0:
                raise ValueError('unexpected RFB server message')
            self.read(1)
            rectangles = struct.unpack('>H', self.read(2))[0]
            if rectangles > 4096:
                raise ValueError('too many RFB rectangles')
            saw_pixels = False
            for _ in range(rectangles):
                x, y, width, height, encoding = struct.unpack('>HHHHi', self.read(12))
                if encoding == -223:
                    self.resize(width, height)
                    coverage = [bytearray(self.width) for _ in range(self.height)]
                    continue
                if encoding != 0 or not width or not height or x+width > self.width or y+height > self.height:
                    raise ValueError('invalid raw RFB rectangle')
                raw = self.read(width*height*4)
                for row in range(height):
                    coverage[y+row][x:x+width] = b"\x01"*width
                    begin = ((y+row)*self.width+x)*4
                    self.pixels[begin:begin+width*4] = raw[row*width*4:(row+1)*width*4]
                saw_pixels = True
            if saw_pixels and all(all(row) for row in coverage):
                return self.width, self.height, bytes(self.pixels)
            # Resize-only update requires a new actual full framebuffer request.
            self.stream.sendall(struct.pack('>BBHHHH', 3, 0, 0, 0, self.width, self.height))
        raise TimeoutError('RFB framebuffer never completed')

    def keys(self, *keysyms):
        if not keysyms or any(type(k) is not int or not 0 <= k <= 0xffffffff for k in keysyms):
            raise ValueError('bounded exact X11 keysyms required')
        try:
            for keysym in keysyms:
                self.stream.sendall(struct.pack('>BBHI', 4, 1, 0, keysym))
            time.sleep(.03)
        finally:
            for keysym in reversed(keysyms):
                self.stream.sendall(struct.pack('>BBHI', 4, 0, 0, keysym))

    def close(self):
        self.stream.close()


def normalized_volumes(volumes):
    # KubeVirt1.8.2 defaults.go setDefaultPullPoliciesOnContainerDisks.
    # Only the digest-pinned default is accepted; all source bytes and explicit
    # policies remain exact. Keep authoritative actual volume bytes in the hash.
    result=json.loads(json.dumps(volumes))
    for volume in result:
        disk=volume.get('containerDisk')
        if disk and re.fullmatch(r'.+@sha256:[0-9a-f]{64}',disk.get('image','')) and 'imagePullPolicy' not in disk:
            disk['imagePullPolicy']='IfNotPresent'
    return result


def vm_identity(vm, vmi):
    """Bind a session to the actual server objects, node and container image."""
    metadata = vm['metadata']
    instance = vmi['metadata']
    if metadata['name'] != instance['name'] or metadata['namespace'] != instance['namespace']:
        raise ValueError('VM/VMI identity mismatch')
    owners = instance.get('ownerReferences', [])
    if not any(owner.get('kind') == 'VirtualMachine' and owner.get('uid') == metadata['uid']
               and owner.get('controller') is True for owner in owners):
        raise ValueError('VMI is not owned by exact VM UID')
    status = vmi.get('status', {})
    if status.get('phase') != 'Running' or not status.get('nodeName'):
        raise ValueError('guest is not running on an identified node')
    declared = vm['spec']['template']['spec']['volumes']
    actual = vmi['spec']['volumes']
    if normalized_volumes(declared) != normalized_volumes(actual):
        raise ValueError('VM/VMI storage sources differ')
    storage_digest = hashlib.sha256(json.dumps(actual,sort_keys=True,separators=(',',':')).encode()).hexdigest()
    return {'namespace': metadata['namespace'], 'name': metadata['name'],
            'vm_uid': metadata['uid'], 'vmi_uid': instance['uid'],
            'storage_sha256': storage_digest, 'node': status['nodeName'], 'migration_state': status.get('migrationState'),
            'container_disks': [volume['containerDisk']['image'] for volume in
                                vm['spec']['template']['spec']['volumes'] if 'containerDisk' in volume]}


def verify_identity(expected, vm, vmi):
    current = vm_identity(vm, vmi)
    if current != expected or current['migration_state'] is not None:
        raise ValueError('guest replaced or migrated during measurement')
    return current
