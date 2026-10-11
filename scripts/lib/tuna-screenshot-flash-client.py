#!/usr/bin/python3
"""Supervised identity exercises the production screenshot admission path."""
import json, os, time
from pathlib import Path
from gi.repository import Gio, GLib
art = Path(os.environ['TUNA_FLASH_PROOF_ARTIFACTS'])
bus = Gio.bus_get_sync(Gio.BusType.SESSION, None)
(art/'client-ready.json').write_text(json.dumps({'pid':os.getpid()}))
seen = None
while True:
    try:
        request=json.loads((art/'request.json').read_text())
    except (OSError,ValueError):
        time.sleep(.01);continue
    if request['id']==seen:
        time.sleep(.01);continue
    seen=request['id']
    started=time.monotonic()
    (art/'started.json').write_text(json.dumps({'id':seen,'monotonic':started}))
    try:
        answer=bus.call_sync('org.gnome.Shell.Screenshot','/org/gnome/Shell/Screenshot','org.gnome.Shell.Screenshot',request['method'],GLib.Variant(request['signature'],tuple(request['args'])),None,Gio.DBusCallFlags.NO_AUTO_START,8000,None)
        response={'id':seen,'answer':answer.unpack()}
    except Exception as error:
        response={'id':seen,'error':str(error)}
    response['elapsed']=time.monotonic()-started
    (art/'response.json').write_text(json.dumps(response))
