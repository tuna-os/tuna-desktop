#!/bin/sh
# Guest probe, run in the user session: settings, portal Lockdown, real portal Camera client, PipeWire state.
echo "## $(date -Is) $1"
for k in disable-camera disable-microphone disable-sound-output; do echo "gsettings $k=$(gsettings get org.gnome.desktop.privacy $k)"; done
for b in gnome tuna; do
  for p in disable-camera disable-microphone disable-sound-output; do
    echo "impl.$b Lockdown $p: $(gdbus call --session -d org.freedesktop.impl.portal.desktop.$b -o /org/freedesktop/portal/desktop -m org.freedesktop.DBus.Properties.Get org.freedesktop.impl.portal.Lockdown $p 2>&1 | head -c 160)"
  done
done
echo "portal Camera.IsCameraPresent: $(gdbus call --session -d org.freedesktop.portal.Desktop -o /org/freedesktop/portal/desktop -m org.freedesktop.DBus.Properties.Get org.freedesktop.portal.Camera IsCameraPresent 2>&1)"
echo "portal Camera.AccessCamera: $(gdbus call --session -d org.freedesktop.portal.Desktop -o /org/freedesktop/portal/desktop -m org.freedesktop.portal.Camera.AccessCamera '{}' 2>&1)"
echo "portal Camera.OpenPipeWireRemote: $(gdbus call --session -d org.freedesktop.portal.Desktop -o /org/freedesktop/portal/desktop -m org.freedesktop.portal.Camera.OpenPipeWireRemote '{}' 2>&1 | head -c 200)"
pw-dump 2>/dev/null | python3 -c '
import json,sys
for o in json.load(sys.stdin):
    if o.get("type")!="PipeWire:Interface:Node": continue
    p=(o.get("info") or {}).get("props") or {}
    if not str(p.get("node.name","")).startswith("tuna-q"): continue
    mute=None
    for prm in ((o.get("info") or {}).get("params") or {}).get("Props") or []:
        if "mute" in prm: mute=prm["mute"]
    print("pw node", o["id"], p.get("node.name"), p.get("media.class"), "mute="+str(mute))
'
for n in tuna-q-sink tuna-q-mic; do id=$(pw-dump 2>/dev/null | python3 -c "import json,sys;print(next((o['id'] for o in json.load(sys.stdin) if ((o.get('info') or {}).get('props') or {}).get('node.name')=='$n'),''))"); [ -n "$id" ] && echo "wpctl get-volume $n($id): $(wpctl get-volume $id 2>&1)"; done
