"""Bind original package bytes to the exact qualified image; no release admission."""
import base64
import hashlib
import json
from pathlib import Path
import re


PROTOCOL_MEMBERS = ('/usr/libexec/tuna-explicit-sync-client',
                    '/usr/libexec/tuna-background-effect-client',
                    '/usr/share/tuna-explicit-sync/protocol-xml.sha256',
                    '/usr/share/tuna-explicit-sync/protocol-source.json')
PROTOCOL_INPUTS = ('scripts/tuna-explicit-sync-build',
                   'scripts/tuna-background-effect-client-build',
                   'scripts/lib/tuna-explicit-sync-client.c',
                   'scripts/lib/tuna-background-effect-client.c')


def fixture_members(fixture):
    if fixture == 'lifecycle':
        return {}
    if fixture != 'explicit-sync':
        raise ValueError('unknown native image fixture')
    return {'/usr/libexec/tuna-vm-explicit-sync': 'scripts/lib/tuna-explicit-sync-guest.py',
            '/usr/libexec/background_effect_oracle.py': 'scripts/lib/background_effect_oracle.py',
            '/usr/share/tuna-explicit-sync/native-build.json': 'native-protocol-build.json'}


def canonical_image_id(observed):
    """Canonicalize Podman's full image ID without admitting tags or short IDs."""
    if not isinstance(observed, str) or not re.fullmatch(r'(?:sha256:)?[0-9a-f]{64}', observed):
        raise ValueError('full immutable Podman image ID required')
    return observed if observed.startswith('sha256:') else 'sha256:' + observed


def validate_base_origin(origin, base_image, index_bytes=None):
    """Bind selection to the actual chosen platform and current raw OCI index."""
    from maintained_media import json_blob, require
    base = origin.get("base", {})
    require(isinstance(base, dict) and
            re.fullmatch(r"sha256:[0-9a-f]{64}", origin.get("base_index", "")) and
            re.fullmatch(r"sha256:[0-9a-f]{64}", origin.get("base_platform", "")) and
            base.get("index_digest") == origin["base_index"] and
            base.get("platform") == origin["base_platform"] and
            base_image == "ghcr.io/tuna-os/marlin:gnome@" + origin["base_platform"],
            "product origin base differs from actual chosen immutable image platform")
    if index_bytes is not None:
        index = json_blob(index_bytes)
        require(index.get("schemaVersion") == 2 and isinstance(index.get("manifests"), list),
                "current base evidence is not an OCI index")
        members = [member for member in index["manifests"]
                   if member.get("platform", {}).get("os") == "linux" and
                   member.get("platform", {}).get("architecture") == "amd64"]
        require(origin["base_index"] == "sha256:" + hashlib.sha256(index_bytes).hexdigest() and
                len(members) == 1 and members[0].get("digest") == origin["base_platform"],
                "product origin differs from actual current raw base index/platform")


def validate_product_origin(origin, image, root):
    """Check the authenticated selection's immutable product and source bindings.

    API authentication belongs to the native product selector. This receipt is
    subsequently sealed inside the trusted CI source artifact, never accepted as
    standalone authorization to load an image.
    """
    from maintained_media import git_source, json_blob, require, sha
    require(isinstance(origin, dict) and
            origin.get("schema") == "tuna-native-product-selection-v1" and
            origin.get("mode") in ("fresh", "reused") and
            origin.get("profile") in ("distribution", "upstream51"), "invalid explicit product origin")
    product = git_source(root, image.get("commit"))
    capture = git_source(root, image.get("capture_commit"))
    require(origin.get("product_source") == product and origin.get("current_capture_source") == capture and
            product["required_source_inputs_sha256"] == capture["required_source_inputs_sha256"] and
            image.get("built_tree") == product["tree"] and image.get("capture_tree") == capture["tree"] and
            image.get("required_source_inputs_sha256") == product["required_source_inputs_sha256"],
            "product origin source differs from actual equivalent Git inputs")
    package = origin.get("package", {})
    require(isinstance(package, dict) and package.get("sha256") == image.get("package_sha256") and
            isinstance(package.get("basename"), str) and
            re.fullmatch(r"[A-Za-z0-9][A-Za-z0-9._+:-]*\.pkg\.tar\.(?:zst|xz|gz)", package["basename"]),
            "product origin original package differs")
    validate_base_origin(origin, image.get("base_image"))
    if origin["mode"] == "fresh":
        require(product == capture and origin.get("image_id") in ("", None, image.get("image_id")) and
                not origin.get("authenticated_origin"), "fresh product cannot claim an older build")
        return
    require(origin.get("image_id") == image.get("image_id"), "reused loaded image differs from origin")
    lineage = origin.get("authenticated_origin", {})
    require(isinstance(lineage, dict) and lineage.get("repository") == "tuna-os/tuna-desktop" and
            lineage.get("workflow_path") == ".github/workflows/ci.yml" and
            all(type(lineage.get(key)) is int and lineage[key] > 0
                for key in ("workflow_id", "run_id", "run_attempt")) and
            sha(lineage.get("head_sha"), 40), "missing authenticated native capsule lineage")
    for key, name in (("pointer_artifact", "native-product-" + origin["profile"]),
                      ("data_artifact", "native-product-DATA-" + origin["profile"])):
        artifact = lineage.get(key, {})
        require(isinstance(artifact, dict) and type(artifact.get("id")) is int and artifact["id"] > 0 and
                artifact.get("name") == name and
                re.fullmatch(r"sha256:[0-9a-f]{64}", artifact.get("digest", "")),
                "invalid authenticated capsule artifact identity")
    provenance = lineage.get("provenance", {})
    require(isinstance(provenance, dict) and sha(provenance.get("sha256")) and
            type(provenance.get("bytes")) is int and provenance["bytes"] > 0,
            "missing authenticated capsule provenance identity")
    original = origin.get("original_image_receipt", {})
    require(isinstance(original, dict) and isinstance(original.get("raw_base64"), str),
            "missing original image admission bytes")
    try:
        raw = base64.b64decode(original["raw_base64"], validate=True)
    except ValueError as error:
        raise ValueError("invalid original image receipt encoding") from error
    require(len(raw) == original.get("bytes") and hashlib.sha256(raw).hexdigest() == original.get("sha256") and
            origin.get("strip_image") == {"sha256": original["sha256"], "bytes": len(raw)},
            "original authenticated image admission byte identity differs")
    prior = json_blob(raw)
    require(prior == original.get("receipt") and prior.get("schema") == 1 and
            all(prior.get(key) == image.get(key) for key in
                ("commit", "image_id", "base_image", "package_sha256", "member_sha256")),
            "actual loaded image/package differs from original capsule admission")
    prior_package, actual_package = prior.get("package_inspection", {}), image.get("package_inspection", {})
    require(isinstance(prior_package, dict) and isinstance(actual_package, dict) and
            prior_package.get("package", {}).get("sha256") == image.get("package_sha256") and
            type(prior_package.get("package", {}).get("size")) is int and
            prior_package["package"]["size"] > 0 and bool(prior_package.get("required_members")),
            "original capsule lacks actual package inspection")
    require(isinstance(prior_package, dict) and isinstance(actual_package, dict) and
            all(prior_package.get(key) == actual_package.get(key) for key in
                ("pkginfo", "required_members", "member_count", "decompressed_bytes")) and
            all(prior_package.get("package", {}).get(key) == actual_package.get("package", {}).get(key)
                for key in ("sha256", "size")),
            "actual original package inspection differs from authenticated capsule")


def admission(package, observed, root, commit, image_id, base_image, *, fixture='lifecycle', capture_commit=None,
              product_origin=None, base_evidence=None):
    if not re.fullmatch(r'[0-9a-f]{40}', commit):
        raise ValueError('exact source commit missing')
    if not re.fullmatch(r'sha256:[0-9a-f]{64}', image_id):
        raise ValueError('immutable final image ID missing')
    if not re.fullmatch(r'ghcr.io/tuna-os/marlin:gnome@sha256:[0-9a-f]{64}', base_image):
        raise ValueError('immutable base image missing')
    if observed.get('schema') != 1 or observed.get('image_id') != image_id:
        raise ValueError('image inspection identity differs')
    expected = {'/' + name: data['sha256'] for name, data in package['required_members'].items()
                if name.startswith('usr/bin/')}
    for name in ('tuna-vm-input', 'tuna-vm-lifecycle', 'tuna-vm-shell-focus'):
        path = Path(root) / 'packaging/marlin/vm-lane' / name
        expected['/usr/libexec/' + name] = hashlib.sha256(path.read_bytes()).hexdigest()
    for installed, source in fixture_members(fixture).items():
        expected[installed] = hashlib.sha256((Path(root) / source).read_bytes()).hexdigest()
    if fixture == 'explicit-sync':
        build = json.loads((Path(root) / 'native-protocol-build.json').read_bytes())
        sources = {name: hashlib.sha256((Path(root) / name).read_bytes()).hexdigest()
                   for name in PROTOCOL_INPUTS}
        if (set(build) != {'schema', 'source_sha256', 'member_sha256'}
                or build['schema'] != 1 or build['source_sha256'] != sources):
            raise ValueError('native protocol build inputs differ from exact source')
        members = build['member_sha256']
        if (type(members) is not dict or set(members) != set(PROTOCOL_MEMBERS)
                or any(not isinstance(value, str) or not re.fullmatch('[0-9a-f]{64}', value)
                       for value in members.values())):
            raise ValueError('native protocol original build member identity missing')
        staged = Path(root) / 'protocol-xml/wayland-protocols/source.json'
        if members['/usr/share/tuna-explicit-sync/protocol-source.json'] != hashlib.sha256(staged.read_bytes()).hexdigest():
            raise ValueError('native protocol XML source differs from independently staged locked archive')
        expected.update(members)
    if observed.get('member_sha256') != expected:
        raise ValueError('final image installed executables differ from package/source bytes')
    digest = package['package']['sha256']
    if not re.fullmatch(r'[0-9a-f]{64}', digest):
        raise ValueError('original package byte hash missing')
    from maintained_media import git_source, require
    import subprocess
    head = subprocess.check_output(["git", "-C", str(root), "rev-parse", "HEAD"], text=True).strip()
    capture_commit = head if capture_commit is None else capture_commit
    require(capture_commit == head, "capture source differs from actual executing checkout")
    product, capture = git_source(root, commit), git_source(root, capture_commit)
    require(product["required_source_inputs_sha256"] == capture["required_source_inputs_sha256"],
            "original product inputs differ from actual capture source")
    require(commit == capture_commit or product_origin is not None,
            "older product requires explicit authenticated product origin")
    receipt = {'schema': 1, 'commit': commit, 'image_id': image_id, 'base_image': base_image,
               'built_tree': product['tree'], 'capture_commit': capture_commit, 'capture_tree': capture['tree'],
               'required_source_inputs_sha256': product['required_source_inputs_sha256'],
               'package_sha256': digest, 'member_sha256': expected,
               'package_inspection': package,
               'installed_signed_release_qualified': False,
               'scope': 'original CI package and exact final image, not signed release or kernel presentation'}
    if product_origin is not None:
        validate_product_origin(product_origin, receipt, root)
        from maintained_media import confined
        require(base_evidence is not None, "explicit product origin requires current base evidence")
        index_path = confined(Path(base_evidence), "index.json")
        require(index_path.stat().st_size <= 1048576, "current base index exceeds admission bound")
        validate_base_origin(product_origin, base_image, index_path.read_bytes())
        receipt['product_origin'] = product_origin
    return receipt


# Executed inside the final immutable image by the existing CI job. No host
# filesystem mounts, guest configuration changes, or credentials are required.
_INSPECTION_BODY = '''result={}
for name in names:
 fd=os.open(name,os.O_RDONLY|os.O_NOFOLLOW)
 try:
  before=os.fstat(fd)
  if not stat.S_ISREG(before.st_mode) or not 0<before.st_size<=67108864: raise ValueError("installed member size/type")
  h=hashlib.sha256()
  while True:
   chunk=os.read(fd,65536)
   if not chunk: break
   h.update(chunk)
  after=os.fstat(fd)
  if (before.st_dev,before.st_ino,before.st_size,before.st_mtime_ns,before.st_ctime_ns)!=(after.st_dev,after.st_ino,after.st_size,after.st_mtime_ns,after.st_ctime_ns): raise ValueError("installed bytes changed")
  result[name]=h.hexdigest()
 finally: os.close(fd)
print(json.dumps({"schema":1,"image_id":sys.argv[1],"member_sha256":result}))
'''


def inspection_program(fixture='lifecycle'):
    names = ['/usr/bin/tuna-' + name for name in
             ('compositor', 'session', 'shell-host', 'shell-gtk', 'ibus-bridge')]
    names += ['/usr/libexec/tuna-vm-input', '/usr/libexec/tuna-vm-lifecycle',
              '/usr/libexec/tuna-vm-shell-focus']
    names += list(fixture_members(fixture))
    if fixture == 'explicit-sync':
        names += list(PROTOCOL_MEMBERS)
    return 'import hashlib,json,os,stat,sys\nnames=' + repr(names) + '\n' + _INSPECTION_BODY
