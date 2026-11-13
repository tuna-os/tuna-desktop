"""Shared exact-image, bounded QGA and original-session native observations."""
import hashlib
import json
import math
from pathlib import Path
import re
import time
import uuid


class NativeDesktop:
    def __init__(self, qmp, agent_factory, out, metadata, *, action, source_members, nonce=None):
        if action not in ('core-apps', 'autotz'):
            raise ValueError('fixed native consumer action required')
        required = ('commit', 'image_id', 'base_image', 'package_sha256', 'member_sha256')
        if not all(key in metadata for key in required):
            raise ValueError('existing exact native image admission required')
        if (metadata.get('schema') != 1 or metadata.get('installed_signed_release_qualified') is not False
                or re.fullmatch('[0-9a-f]{40}', metadata['commit']) is None
                or re.fullmatch('[0-9a-f]{64}', metadata['package_sha256']) is None
                or re.fullmatch('(sha256:)?[0-9a-f]{64}', metadata['image_id']) is None
                or re.fullmatch('.+@sha256:[0-9a-f]{64}', metadata['base_image']) is None
                or not isinstance(metadata['member_sha256'], dict)):
            raise ValueError('malformed immutable DEV image/package admission')
        self._observer_members = {name: hashlib.sha256(path.read_bytes()).hexdigest()
                                  for name, path in source_members.items()}
        if any(metadata['member_sha256'].get(name) != digest
               for name, digest in self._observer_members.items()):
            raise ValueError('installed native observer image members differ from executing source')
        self.qmp, self._agent_factory = qmp, agent_factory
        self.out, self.metadata = Path(out), dict(metadata)
        self.out.mkdir(parents=True, exist_ok=True)
        self.nonce = nonce or uuid.uuid4().hex
        if re.fullmatch('[0-9a-f]{32}', self.nonce) is None:
            raise ValueError('exact original native fixture nonce required')
        self._action = action
        self.width, self.height, self.scale = 1280, 800, 1.0
        self._deadline = self._uncertain = None
        self._sequence = 0
        self.installed_signed_release_qualified = False
        self.fixture = None

    def retain(self, label, value):
        if not re.fullmatch(r'[A-Za-z0-9_.-]{1,120}', label):
            raise ValueError('bounded public evidence label required')
        self._sequence += 1
        path = self.out / f'{self._sequence:05d}-{label}.json'
        path.write_text(json.dumps(value, indent=2, ensure_ascii=False) + '\n')
        return str(path)

    def _execute(self, action, arguments, label, request):
        if self._uncertain is not None:
            raise RuntimeError('previous owned guest-exec child retirement unconfirmed; VM retirement required')
        deadline = time.monotonic() + 30
        if self._deadline is not None:
            deadline = min(deadline, self._deadline)
        agent = None
        try:
            if time.monotonic() >= deadline:
                raise TimeoutError('native action absolute phase deadline expired before acquisition')
            agent = self._agent_factory(deadline=deadline)
            remaining = deadline - time.monotonic()
            if remaining <= 0:
                raise TimeoutError('native action absolute phase deadline expired during acquisition')
            value = agent.run(action, *arguments, timeout=remaining)
            if time.monotonic() >= deadline:
                raise TimeoutError('native action completed outside its original absolute deadline')
            self.retain(label, {'request': request, 'observed': value})
            return value
        except Exception as error:
            state = getattr(agent, 'guest_exec_state', 'none')
            failure = {'request': request, 'error_type': type(error).__name__,
                       'message': str(error)[:2000],
                       'pending_guest_pid': getattr(agent, 'pending_guest_pid', None),
                       'guest_exec_state': state, 'not_replayed': True,
                       'installed_signed_release_qualified': False}
            if state != 'none':
                self._uncertain = failure
            self.retain('failed-' + label, failure)
            raise
        finally:
            if agent is not None:
                agent.close()

    def guest(self, domain, operation, **arguments):
        request = {'domain': domain, 'operation': operation,
                   'arguments': arguments, 'run_nonce': self.nonce}
        return self._execute(self._action, (domain, operation,
                             json.dumps(arguments | {'_nonce': self.nonce}, separators=(',', ':'))),
                             f'guest-{domain}-{operation}', request)

    def await_state(self, label, read, predicate, timeout=10):
        if timeout not in (10, 30):
            raise ValueError('readiness budget must be explicitly designed as 10 or 30 seconds')
        previous = self._deadline
        deadline = time.monotonic() + timeout
        if previous is not None:
            deadline = min(deadline, previous)
        self._deadline = deadline
        observed = None
        try:
            while time.monotonic() < deadline:
                observed = read()
                if time.monotonic() >= deadline:
                    break
                if predicate(observed):
                    self.retain(label, {'observed': observed, 'readiness_timeout_sec': timeout})
                    return observed
                time.sleep(min(0.15, max(0, deadline - time.monotonic())))
            self.retain('failed-' + label, {'last_observation': observed,
                                           'readiness_timeout_sec': timeout})
            raise TimeoutError('actual readiness absolute deadline expired: ' + label)
        finally:
            self._deadline = previous

    def state(self, unlocked=True):
        state = self.guest('desktop', 'state')
        if unlocked and state['locked']:
            raise RuntimeError('ordinary app input/observation refused while locked')
        return state

    def tree(self, actor):
        return self.guest('desktop', 'tree', actor=actor)

    def observe(self, label, actor=None):
        value = self.tree(actor) if actor is not None else {'state': self.state()}
        frame = self.out / f'{self._sequence + 1:05d}-{label}.png'
        self.qmp.frame(str(frame))
        value['frame_path'] = str(frame)
        if actor is not None and actor.get('kind') != 'shell':
            rect = self._window(actor, value['state'])['rect']
            left, top = max(0, math.floor(rect[0] * self.scale)), max(0, math.floor(rect[1] * self.scale))
            right = min(self.width, math.ceil((rect[0] + rect[2]) * self.scale))
            bottom = min(self.height, math.ceil((rect[1] + rect[3]) * self.scale))
            if right <= left or bottom <= top:
                raise RuntimeError('original app has no actual visible physical frame region')
            value['frame_bounds'] = [left, top, right - left, bottom - top]
            value['window_logical_rect'], value['output_scale'] = rect, self.scale
        value['boot_identity'] = self.inventory['identity']
        value['source'] = self.metadata
        self.retain(label, value)
        return value

    def shell_actor(self):
        return self.guest('desktop', 'shell-actor')
