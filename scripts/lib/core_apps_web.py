"""Actual native Maps, Weather, Web, Characters, Clocks, Tour and Help.

All product operations are ordinary original-user input through Desktop. Guest
reads independently inspect installed content, real service owners and resulting
payloads. External provider failure is fatal, never a canned-content fallback.
"""
from __future__ import annotations

import base64
import re
import time


def require(condition, message):
    if not condition:
        raise RuntimeError(message)


def nodes(observation):
    value = observation.get("nodes")
    require(isinstance(value, list), "incomplete owner-bound accessibility observation")
    showing = [node for node in value if node.get("showing")]
    frames = [node for node in showing if node.get("role") in ("frame", "dialog", "window") and node.get("bounds")]
    visible = []
    for node in showing:
        rectangle = node.get("bounds")
        ancestors = [frame for frame in frames if node.get("path", [])[:len(frame["path"])] == frame["path"]]
        if rectangle and ancestors:
            frame = max(ancestors, key=lambda item: len(item["path"]))
            x, y, width, height = rectangle
            _, _, frame_width, frame_height = frame["bounds"]
            # Offscreen carousel pages and clipped search results may remain
            # mapped in AT-SPI; they are not actual rendered principal content.
            if width <= 0 or height <= 0 or not (0 <= x + width / 2 < frame_width and 0 <= y + height / 2 < frame_height):
                continue
        visible.append(node)
    return visible


def words(observation, include_entries=False):
    result = []
    for node in nodes(observation):
        if node.get("role") == "password text" or (not include_entries and node.get("role") in ("entry", "search box")):
            continue
        for key in ("name", "text"):
            value = node.get(key)
            if isinstance(value, str) and value and value not in result:
                result.append(value)
    return "\n".join(result)


def content(desktop, actor, label, *phrases, timeout=10):
    return desktop.await_state(label, lambda: desktop.observe(label, actor),
                               lambda row: all(" ".join(phrase.split()) in " ".join(words(row).split()) for phrase in phrases), timeout=timeout)


def unique(observation, predicate, description, outermost=False):
    matches = [node for node in nodes(observation) if predicate(node)]
    if len(matches) != 1:
        # A container proxy (activatable row) and the leaf widget it forwards
        # to can share one name and role. Pointer clicks drive the precise
        # leaf; AT-SPI actions drive the proxy row, whose activation the
        # toolkit forwards while a leaf indicator may expose no action at
        # all. Genuinely distinct duplicates still fail below either way.
        paths = [list(node.get("path", [])) for node in matches]
        if outermost:
            keeps = [node for node, path in zip(matches, paths)
                     if not any(other != path and path[:len(other)] == other for other in paths)]
        else:
            keeps = [node for node, path in zip(matches, paths)
                     if not any(other != path and other[:len(path)] == path for other in paths)]
        if len(keeps) == 1:
            return keeps[0]
    require(len(matches) == 1, f"expected unique actual {description}, observed {len(matches)}")
    return matches[0]


def control(desktop, actor, names, roles=None, outermost=False):
    tree = desktop.tree(actor)
    if isinstance(names, str):
        names = (names,)
    if roles is None:
        roles = ("push button", "button", "toggle button", "page tab", "menu item", "check menu item",
                 "radio menu item", "link", "list item", "row", "check box", "switch")
    return unique(tree, lambda n: n.get("name") in names and n.get("sensitive") and
                  (roles is None or n.get("role") in roles), "/".join(names), outermost=outermost)


def press(desktop, actor, names, roles=None):
    node = control(desktop, actor, names, roles, outermost=True)
    return desktop.press(actor, node["name"], role=node["role"])


def field(desktop, actor, name=None):
    return unique(desktop.tree(actor), lambda n: n.get("role") in ("text", "entry", "search box", "spin button")
                  and n.get("editable") and n.get("text") is not None and n.get("sensitive")
                  and (name is None or n.get("name") == name), "editable " + str(name))


def replace_field(desktop, actor, value, name=None):
    node = field(desktop, actor, name)
    desktop.click_path(actor, node["path"])
    desktop.keys("ctrl", "a")
    if value:
        desktop.type_ascii(value)
    else:
        desktop.keys("backspace")
    return desktop.await_state("actual-keyboard-field", lambda: desktop.tree(actor),
                               lambda row: any(n.get("path") == node["path"] and n.get("focused") and n.get("text") == value
                                               for n in nodes(row)))


def clipboard_text(desktop, expected):
    receipt = desktop.clipboard()
    value = base64.b64decode(receipt["base64"], validate=True)
    require(value == expected.encode("utf-8"), "actual native clipboard does not contain exact unchanged Unicode bytes")
    return receipt


def _navigate(desktop, actor, url):
    desktop.select(actor)
    desktop.keys("ctrl", "l")
    desktop.type_ascii(url)
    desktop.keys("ret")


def _browser(desktop, fixture, endpoint):
    actor = desktop.start("org.gnome.Epiphany", "Web")
    evidence = {"endpoint": endpoint}
    url, marker = endpoint["url"], fixture["marker"]
    _navigate(desktop, actor, url)
    evidence["loaded"] = content(desktop, actor, "web-real-http-content", "Browser content " + marker, "Stored payload: unset", timeout=30)
    empty_before = [row for row in desktop.guest("web", "endpoint")["requests"]
                    if row["path"] == "/" + desktop.nonce + "/submitted"]
    press(desktop, actor, "Submit actual payload")
    evidence["required_field_negative"] = content(desktop, actor, "web-required-field-no-submit", "Browser content " + marker)
    after_invalid = desktop.guest("web", "endpoint")["requests"]
    evidence["invalid_form_independent_requests"] = {"before": empty_before, "after": after_invalid}
    require([row for row in after_invalid if row["path"] == "/" + desktop.nonce + "/submitted"] == empty_before,
            "invalid empty required form unexpectedly reached the ordinary HTTP consumer")
    replace_field(desktop, actor, "cobalt-" + desktop.nonce, "Qualification payload")
    desktop.keys("ctrl", "a")
    desktop.keys("ctrl", "c")
    evidence["keyboard_copy"] = clipboard_text(desktop, "cobalt-" + desktop.nonce)
    replace_field(desktop, actor, "", "Qualification payload")
    desktop.keys("ctrl", "v")
    desktop.await_state("web-actual-paste", lambda: desktop.tree(actor),
                        lambda row: any(n.get("text") == "cobalt-" + desktop.nonce for n in nodes(row)))
    payload_field = field(desktop, actor, "Qualification payload")
    evidence["ime"] = desktop.ime(actor, payload_field["name"], field_path=payload_field["path"])
    evidence["ime_clipboard_before_paste"] = clipboard_text(desktop, "你好")
    replace_field(desktop, actor, "", "Qualification payload")
    desktop.keys("ctrl", "v")
    desktop.await_state("web-real-ime-output-form-consumer", lambda: desktop.tree(actor),
                        lambda row: any(n.get("text") == "你好" and n.get("focused") for n in nodes(row)))
    ime_value = field(desktop, actor, "Qualification payload")["text"]
    require(ime_value == "你好", "Web native form did not consume exact real IME clipboard output")
    desktop.keys("ctrl", "a")
    desktop.keys("ctrl", "c")
    evidence["ime_clipboard"] = clipboard_text(desktop, ime_value)
    press(desktop, actor, "Submit actual payload")
    evidence["form"] = content(desktop, actor, "web-actual-form-consumer", "Submitted " + marker, "Received exact payload: " + ime_value)
    requests = desktop.guest("web", "endpoint")["requests"]
    submitted = [r for r in requests if r["path"] == "/" + desktop.nonce + "/submitted"]
    require(len(submitted) == 1 and submitted[0]["query"] == {"payload": [ime_value]}, "server readback differs from actually committed form bytes")
    require("AppleWebKit" in submitted[0]["user_agent"], "form request did not come from the real WebKit consumer")
    press(desktop, actor, "Return to content", ("link",))
    content(desktop, actor, "web-localstorage-real-readback", "Stored payload: " + ime_value)
    press(desktop, actor, "Second real page", ("link",))
    evidence["navigation"] = content(desktop, actor, "web-real-link-navigation", "Second page " + marker, "silver meadow 6427")
    desktop.keys("alt", "left")
    content(desktop, actor, "web-real-history-back", "Browser content " + marker, "Stored payload: " + ime_value)
    _navigate(desktop, actor, url + "missing")
    evidence["network_negative"] = content(desktop, actor, "web-real-http-404", "404", "No qualification content at this path", timeout=30)
    desktop.keys("alt", "left")
    content(desktop, actor, "web-http-negative-recovery", "Browser content " + marker)
    evidence["open_file"] = desktop.open_file(actor, fixture["local_file"], cancel_first=True)
    content(desktop, actor, "web-native-file-content", "Local file " + marker, "cobalt orchard 7391")
    evidence["default_handler"] = desktop.open_default(fixture["default_file"])
    desktop.select(actor)
    content(desktop, actor, "web-actual-default-handler", "Default file " + marker, "amber harbor 5283")
    _navigate(desktop, actor, url)
    content(desktop, actor, "web-before-crosscut", "Stored payload: " + ime_value)

    def probe(current, stage):
        row = content(desktop, current, "web-" + stage, "Browser content " + marker, "Stored payload: " + ime_value,
                      "Cookie readback: tuna_native=" + desktop.nonce, timeout=30)
        state = desktop.guest("web", "state", app="org.gnome.Epiphany")
        require(any("/submitted?" in h["url"] and h["visits"] > 0 for h in state["history"]), "actual browser history did not persist form navigation")
        return {"rendered_content": row, "independent_history": state}

    def restarted(current, stage):
        _navigate(desktop, current, url)
        return probe(current, stage)

    evidence["crosscut"] = desktop.crosscut(actor, probe, restart_probe=restarted, ime_field="Qualification payload")
    return desktop.result(actor, "real-http-form-navigation-file-default-handler-cookie-localstorage", evidence,
                          installed_signed_release_qualified=False)


def _select_snowman(desktop, actor, label):
    tree = desktop.tree(actor)
    # CharactersView is a custom Gtk.Widget, not fictitious accessible glyphs.
    leaves = [n for n in nodes(tree) if n.get("role") in ("drawing area", "unknown", "panel", "filler")
              and len(n.get("bounds", [])) == 4 and n["bounds"][2] >= 250 and n["bounds"][3] >= 150
              and not any(other.get("path", [])[:len(n["path"])] == n["path"] and len(other.get("path", [])) > len(n["path"])
                          for other in nodes(tree))]
    require(len(leaves) == 1, "cannot uniquely locate actual upstream Characters glyph canvas")
    canvas = leaves[0]
    width, height = canvas["bounds"][2:]
    columns = int(width // 100)
    first_x = (width - columns * 100) / 2 + 50
    desktop.click_at(actor, canvas["path"], first_x / width, min(50 / height, 0.5))
    return content(desktop, actor, label, "Snowman", "U+2603")


def _characters(desktop):
    actor = desktop.start("org.gnome.Characters", "Characters")
    evidence = {}
    prior = desktop.clipboard()
    desktop.keys("ctrl", "f")
    replace_field(desktop, actor, "tuna-no-unicode-result-" + desktop.nonce)
    evidence["negative"] = content(desktop, actor, "characters-negative-search", "No Results", "Try a different search.")
    require(desktop.clipboard()["base64"] == prior["base64"], "negative Characters search changed actual clipboard")
    search = field(desktop, actor)
    evidence["ime"] = desktop.ime(actor, search["name"], field_path=search["path"])
    replace_field(desktop, actor, "snowman")
    desktop.await_state("characters-real-search-ready", lambda: desktop.tree(actor),
                        lambda row: "Loading…" not in words(row) and "No Results" not in words(row))
    evidence["selection"] = _select_snowman(desktop, actor, "characters-selected-codepoint")
    press(desktop, actor, "Copy Character")
    evidence["copy"] = clipboard_text(desktop, "☃")
    desktop.keys("esc")
    desktop.keys("esc")
    recent = control(desktop, actor, "Recently Used", ("list item", "row"))
    desktop.click_path(actor, recent["path"])
    state = desktop.guest("web", "state", app=actor["id"])
    require("☃" in state["values"]["recent-characters"], "actual Unicode selection did not persist Recent Characters")
    evidence["stored"] = state
    # Actual readback through recent selection, including after process restart.
    def probe(current, stage):
        row = desktop.observe("characters-" + stage, current)
        saved = desktop.guest("web", "state", app=current["id"])
        require(saved["values"]["recent-characters"][0] == "☃", "actual first recent codepoint differs")
        dialog = _select_snowman(desktop, current, "characters-recent-consumer-" + stage)
        press(desktop, current, "Copy Character")
        copied = clipboard_text(desktop, "☃")
        desktop.keys("esc")
        return {"recent_ui": row, "rendered_exact_codepoint": dialog, "recent_exact_unicode": saved, "clipboard": copied}
    def restarted(current, stage):
        recent = control(desktop, current, "Recently Used", ("list item", "row"))
        desktop.click_path(current, recent["path"])
        return probe(current, stage)
    evidence["crosscut"] = desktop.crosscut(actor, probe, restart_probe=restarted)
    return desktop.result(actor, "unicode-search-select-exact-copy-negative-and-recent-persistence", evidence,
                          installed_signed_release_qualified=False)


def settings_panel(desktop, title, actor=None, result_names=None):
    """Open a real Settings result using its observed Search button, not Ctrl+F."""
    actor = actor or desktop.start("org.gnome.Settings", "Settings")
    require(actor["id"] == "org.gnome.Settings", "Settings search requires its original native actor")
    search = control(desktop, actor, ("Search", "Search Settings"),
                     ("push button", "button", "toggle button"))
    desktop.click_path(actor, search["path"])
    focused = desktop.await_state("settings-native-search-focused", lambda: desktop.tree(actor),
        lambda row: any(n.get("focused") and n.get("sensitive") and
                       n.get("role") in ("text", "entry", "search box") for n in nodes(row)))
    entry = unique(focused, lambda n: n.get("focused") and n.get("sensitive") and
                   n.get("role") in ("text", "entry", "search box"), "focused native Settings search entry")
    desktop.keys("ctrl", "a")
    desktop.type_ascii(title)
    desktop.await_state("settings-physical-search-query", lambda: desktop.tree(actor),
        lambda row: any(n.get("path") == entry["path"] and n.get("focused") and n.get("text") == title for n in nodes(row)))
    names = tuple(result_names) if result_names is not None else (title,)
    roles = ("list item", "row", "push button", "button")
    results = desktop.await_state("settings-native-search-results", lambda: desktop.tree(actor),
        lambda row: any(n.get("name") in names and n.get("role") in roles and n.get("sensitive") for n in nodes(row)))
    candidate = unique(results, lambda n: n.get("name") in names and n.get("role") in roles and n.get("sensitive"),
                       "actual Settings search result " + title)
    desktop.click_path(actor, candidate["path"])
    return actor


def _location_settings(desktop):
    actor = settings_panel(desktop, "Location", result_names=("Location", "Location Services"))
    desktop.await_state("actual-location-policy-page", lambda: desktop.tree(actor),
                        lambda row: any(n.get("role") in ("switch", "toggle button", "check box") for n in nodes(row)))
    return actor


def _switch(desktop, settings, title, enabled):
    observation = desktop.tree(settings)
    titles = (title,) if isinstance(title, str) else title
    named = [n for n in nodes(observation) if n.get("name") in titles]
    candidates = []
    for node in nodes(observation):
        if node.get("role") not in ("switch", "toggle button", "check box") or not node.get("sensitive"):
            continue
        if node.get("name") in titles or any(node.get("path", [])[:len(row["path"])] == row["path"] for row in named):
            candidates.append(node)
    require(len(candidates) == 1, "actual Location policy switch is missing or ambiguous: " + str(titles))
    node = candidates[0]
    require(isinstance(node.get("checked"), bool), "Location switch lacks actual checked state")
    if node["checked"] != enabled:
        desktop.click_path(settings, node["path"])
    return desktop.await_state("location-policy-ui-change", lambda: desktop.tree(settings),
                               lambda row: any(n.get("path") == node["path"] and n.get("checked") == enabled for n in nodes(row)))


def _master(desktop, settings, enabled, app):
    desktop.select(settings)
    result = _switch(desktop, settings, ("Automatic Device Location", "Location Services"), enabled)
    actual = desktop.guest("web", "state", app=app)
    require(actual["location_master"] is enabled, "actual location master readback differs from Settings UI")
    return {"ui": result, "independent": actual}


def _consent(desktop, app):
    shell = desktop.shell_actor()
    observed = desktop.await_state("real-location-consent-or-existing-grant",
        lambda: {"shell": desktop.tree(shell), "permission": desktop.guest("web", "state", app=app)},
        lambda row: any(n.get("name") == "Allow" and n.get("sensitive") for n in nodes(row["shell"])) or
                    row["permission"]["location_permissions"].get(app, [None])[0] in ("EXACT", "CITY", "STREET", "NEIGHBORHOOD"))
    tree = observed["shell"]
    if any(n.get("name") == "Allow" and n.get("sensitive") for n in nodes(tree)):
        display_name = {"org.gnome.Maps": "Maps", "org.gnome.Weather": "Weather"}[app]
        require(app in words(tree, include_entries=True) or display_name in words(tree, include_entries=True),
                "native shell consent belongs to a different app")
        press(desktop, shell, "Allow")
        return {"prompt": tree, "permission": desktop.await_state("real-location-grant-stored",
            lambda: desktop.guest("web", "state", app=app),
            lambda row: row["location_permissions"].get(app, [None])[0] in ("EXACT", "CITY", "STREET", "NEIGHBORHOOD"))}
    return observed


def _same_actor(observed, actor):
    return observed is not None and observed.get("available", True) and all(
        observed.get(key) == actor[key] for key in ("pid", "uid", "start_ticks", "exe_sha256"))


def _fix_for(row, actor):
    fixes = [client for client in row["clients"] if _same_actor(client["actor"], actor) and
             (client["properties"].get("DesktopId") == actor["id"] or
              client.get("desktop_id_set", {}).get("requested") == actor["id"]) and
             client["properties"].get("Active") and "location" in client and "start_reply" in client]
    if len(fixes) != 1:
        return False
    fix = fixes[0]["location"]
    return (abs(fix.get("Latitude", 1000) - 40.6893129) < .01 and
            abs(fix.get("Longitude", 1000) + 74.0445531) < .01 and fix.get("Accuracy", 0) > 0)


def _active_fix(desktop, actor):
    row = desktop.await_state("real-geoclue-client-fix", lambda: desktop.guest("web", "location"),
                              lambda value: _fix_for(value, actor), timeout=30)
    require(row["service"]["available"] and "geoclue" in row["service"]["package"].lower(), "wrong real geolocation service owner")
    return row


def _not_active(desktop, actor, distinction):
    denied_names = ("org.freedesktop.DBus.Error.AccessDenied", "org.freedesktop.GeoClue2.Error.NotAuthorized")
    def observed_negative(value):
        if any(_same_actor(client["actor"], actor) and client["properties"].get("Active") for client in value["clients"]):
            return False
        errors = [event for event in value["consumer_errors"] if _same_actor(event.get("consumer"), actor)]
        if distinction == "unavailable-service":
            return not value["service"]["available"] and any(event["error_name"] not in denied_names for event in errors)
        return value["service"]["available"] and any(event["error_name"] in denied_names for event in errors)
    row = desktop.await_state("geoclue-" + distinction, lambda: desktop.guest("web", "location"), observed_negative)
    require(row["service"]["available"] is (distinction != "unavailable-service"), "GeoClue denied/unavailable distinctions were conflated")
    return row


def _policy_paths(desktop, actor, search_name, settings, principal):
    app = actor["id"]
    evidence = {"positive": _active_fix(desktop, actor)}
    saved = desktop.guest("web", "state", app=app)
    permission = saved["location_permissions"].get(app)
    require(permission and permission[0] in ("EXACT", "CITY", "STREET", "NEIGHBORHOOD"), "actual shell permission store lacks positive app grant")
    try:
        desktop.select(settings)
        evidence["deny_ui"] = _switch(desktop, settings, search_name, False)
        denied = desktop.guest("web", "state", app=app)
        require(denied["location_permissions"].get(app, [None])[0] == "NONE", "per-app UI deny did not persist exact permission denial")
        actor = desktop.restart(actor, search_name)
        evidence["denied_service"] = _not_active(desktop, actor, "permission-denied")
        evidence["denied_principal"] = principal(actor, "permission-denied")
        desktop.select(settings)
        evidence["grant_ui"] = _switch(desktop, settings, search_name, True)
        actor = desktop.restart(actor, search_name)
        _consent(desktop, app)
        evidence["grant_recovery"] = _active_fix(desktop, actor)
        evidence["master_off"] = _master(desktop, settings, False, app)
        actor = desktop.restart(actor, search_name)
        evidence["master_denial"] = _not_active(desktop, actor, "master-disabled")
        evidence["master_principal"] = principal(actor, "master-disabled")
        evidence["master_on"] = _master(desktop, settings, True, app)
        actor = desktop.restart(actor, search_name)
        _consent(desktop, app)
        evidence["master_recovery"] = _active_fix(desktop, actor)
        evidence["outage"] = desktop.guest("web", "location-stop")
        try:
            actor = desktop.restart(actor, search_name)
            evidence["unavailable"] = _not_active(desktop, actor, "unavailable-service")
            evidence["unavailable_principal"] = principal(actor, "unavailable-service")
        finally:
            evidence["service_restoration"] = desktop.guest("web", "location-restore")
        actor = desktop.restart(actor, search_name)
        _consent(desktop, app)
        evidence["restart_recovery"] = _active_fix(desktop, actor)
        evidence["recovered_principal"] = principal(actor, "recovered")
    finally:
        desktop.select(settings)
        _switch(desktop, settings, search_name, True)
        _master(desktop, settings, saved["location_master"], app)
    return actor, evidence


def _maps(desktop, settings, endpoint):
    actor = desktop.start("org.gnome.Maps", "Maps")
    evidence = {"consent": _consent(desktop, "org.gnome.Maps"), "named_location_source": "official-geoclue-static-NewYork-not-physical-GPS"}
    evidence["fix"] = _active_fix(desktop, actor)
    press(desktop, actor, "Current Location")
    desktop.keys("ctrl", "f")
    search = field(desktop, actor)
    evidence["ime"] = desktop.ime(actor, search["name"], field_path=search["path"])
    replace_field(desktop, actor, "Eiffel Tower")
    results = desktop.await_state("maps-real-geocoding-results", lambda: desktop.tree(actor),
        lambda row: any(n.get("sensitive") and n.get("role") in ("list item", "row") and "Eiffel Tower" in n.get("name", "") for n in nodes(row)), timeout=30)
    result = unique(results, lambda n: n.get("sensitive") and n.get("role") in ("list item", "row")
                    and "Eiffel Tower" in n.get("name", ""), "network geocoded Eiffel Tower result")
    desktop.click_path(actor, result["path"])
    evidence["search"] = content(desktop, actor, "maps-real-geographic-content", "Eiffel Tower", timeout=30)
    old_clipboard = desktop.clipboard()
    press(desktop, actor, ("Share Location", "Share location"))
    share = desktop.observe("maps-real-share-content", actor)
    summary = unique(share, lambda n: n.get("role") == "label" and "Eiffel Tower" in (n.get("text") or n.get("name", ""))
                     and "\n" in (n.get("text") or n.get("name", "")), "rendered geographic share summary")
    link = unique(share, lambda n: n.get("role") == "link" and n.get("name", "").startswith("https://openstreetmap.org/"), "actual OSM geographic URI")
    expected_copy = (summary.get("text") or summary["name"]) + "\n" + link["name"]
    press(desktop, actor, "Cancel")
    require(desktop.clipboard()["base64"] == old_clipboard["base64"], "cancelled Maps sharing changed clipboard")
    press(desktop, actor, ("Share Location", "Share location"))
    press(desktop, actor, "Copy")
    evidence["geographic_clipboard"] = clipboard_text(desktop, expected_copy)
    press(desktop, actor, "Current Location")
    evidence["handler_before"] = desktop.await_state("maps-current-location-before-uri-handler",
        lambda: desktop.guest("web", "state", app=actor["id"]),
        lambda row: abs(row["values"]["last-viewed-location"][0] - 40.6893129) < .01, timeout=30)
    browser = desktop.start("org.gnome.Epiphany", "Web")
    _navigate(desktop, browser, endpoint["url"])
    content(desktop, browser, "maps-uri-handler-source", "Browser content TUNA-WEB-" + desktop.nonce)
    press(desktop, browser, "Open geographic handler", ("link",))
    content(desktop, browser, "maps-external-handler-consent", "Open in External App?")
    press(desktop, browser, "Open Link")
    desktop.select(actor)
    evidence["handler_after"] = desktop.await_state("maps-native-geo-uri-consumer",
        lambda: desktop.guest("web", "state", app=actor["id"]),
        lambda row: abs(row["values"]["last-viewed-location"][0] - 48.85837) < .01 and abs(row["values"]["last-viewed-location"][1] - 2.29448) < .01, timeout=30)
    desktop.close(browser)
    desktop.select(actor)
    evidence["network_tiles"] = desktop.guest("web", "map-tiles")
    zoom_before = desktop.guest("web", "state", app=actor["id"])["values"]["zoom-level"]
    desktop.keys("ctrl", "equal")
    evidence["keyboard_zoom"] = desktop.observe("maps-actual-keyboard-zoom", actor)
    persisted = desktop.await_state("maps-real-keyboard-zoom-persisted", lambda: desktop.guest("web", "state", app=actor["id"]),
                                   lambda row: row["values"]["zoom-level"] == zoom_before + 1)
    coords = persisted["values"]["last-viewed-location"]
    require(len(coords) == 2 and abs(coords[0] - 48.85837) < .1 and abs(coords[1] - 2.29448) < .1, "Maps actual UI search did not persist real geographic coordinates")
    evidence["persistence"] = persisted

    def principal(current, stage):
        if stage in ("permission-denied", "master-disabled", "unavailable-service"):
            desktop.select(current)
            tree = desktop.tree(current)
            action = unique(tree, lambda n: n.get("name") == "Current Location" and
                            n.get("role") in ("button", "push button"), "real current-location negative control")
            if action["sensitive"]:
                desktop.press(current, action["name"], role=action["role"])
                expected = "Failed to connect to location service" if stage == "unavailable-service" else "Turn on location services"
                negative = content(desktop, current, "maps-" + stage, expected)
            else:
                negative = {"disabled_actual_action": action,
                            "scene": desktop.observe("maps-disabled-current-location-" + stage, current)}
        else:
            negative = None
        state = desktop.guest("web", "state", app=current["id"])
        observed = state["values"]["last-viewed-location"]
        require(len(observed) == 2 and all(abs(a - b) < 1e-5 for a, b in zip(observed, coords)), "location failure unexpectedly mutated searched Maps viewport")
        return {"ui": desktop.observe("maps-content-" + stage, current), "negative_ui": negative, "actual_coordinates": state}

    actor, evidence["location_paths"] = _policy_paths(desktop, actor, "Maps", settings, principal)

    def probe(current, stage):
        desktop.select(current)
        principal_state = principal(current, stage)
        press(desktop, current, "Main Menu")
        press(desktop, current, "Export as Image…")
        press(desktop, current, "Export")
        destination = desktop.fixture + "/web/map-" + stage + ".png"
        chosen = desktop.portal(current, interface="FileChooser", response="accept", path=destination, save=True)
        exported = desktop.await_state("maps-actual-exported-image-" + stage,
            lambda: desktop.guest("web", "map-image", stage=stage), lambda row: row.get("ready"), timeout=30)
        return {"principal": principal_state, "export_picker": chosen, "real_rendered_map": exported,
                "service": _active_fix(desktop, current)}

    evidence["crosscut"] = desktop.crosscut(actor, probe)
    return desktop.result(actor, "real-network-map-search-render-export-geoclue-policy-and-recovery", evidence,
                          installed_signed_release_qualified=False, physical_geolocation_qualified=False)


def _weather_values(observation):
    text = words(observation)
    return [float(value.replace("−", "-")) for value in re.findall(r"([−-]?\d+(?:\.\d+)?)\s*°", text)]


def _weather(desktop, settings):
    actor = desktop.start("org.gnome.Weather", "Weather")
    evidence = {"consent": _consent(desktop, "org.gnome.Weather"), "named_location_source": "official-geoclue-static-NewYork-not-physical-GPS"}
    evidence["fix"] = _active_fix(desktop, actor)
    maps = desktop.start("org.gnome.Maps", "Maps")
    desktop.keys("ctrl", "f")
    replace_field(desktop, maps, "Eiffel Tower")
    places = desktop.await_state("weather-handoff-real-map-place", lambda: desktop.tree(maps),
        lambda row: any(n.get("role") in ("list item", "row") and "Eiffel Tower" in n.get("name", "") for n in nodes(row)), timeout=30)
    place = unique(places, lambda n: n.get("role") in ("list item", "row") and "Eiffel Tower" in n.get("name", ""), "actual geographic handoff place")
    desktop.click_path(maps, place["path"])
    press(desktop, maps, ("Share Location", "Share location"))
    transfer = desktop.await_state("weather-real-geographic-handoff-control", lambda: desktop.tree(maps),
                                  lambda row: any(n.get("role") in ("list item", "row") and n.get("name") == "Add Paris to Weather" for n in nodes(row)))
    target = unique(transfer, lambda n: n.get("role") in ("list item", "row") and n.get("name") == "Add Paris to Weather", "actual Maps to Weather location action")
    desktop.click_path(maps, target["path"])
    desktop.select(actor)
    evidence["geographic_handler"] = desktop.await_state("weather-actual-location-action-consumer",
        lambda: desktop.observe("weather-actual-location-action-consumer", actor),
        lambda row: "Paris" in words(row) and bool(_weather_values(row)), timeout=30)
    desktop.close(maps)
    desktop.select(actor)
    # Current-location startup may already select New York; the actual city
    # popover remains the application-supported search and IME surface.
    city_button = control(desktop, actor, ("Search for a city or country", "New York", "Paris", "Places"), ("push button", "button", "toggle button"))
    desktop.click_path(actor, city_button["path"])
    search = field(desktop, actor)
    evidence["ime"] = desktop.ime(actor, search["name"], field_path=search["path"])
    replace_field(desktop, actor, "tuna-no-real-city-" + desktop.nonce)
    negative_state = desktop.guest("web", "state", app=actor["id"])
    desktop.keys("ret")
    evidence["unmatched_search_ui"] = desktop.observe("weather-unmatched-city-not-accepted", actor)
    require(desktop.guest("web", "state", app=actor["id"])["settings"]["locations"] == negative_state["settings"]["locations"],
            "actual Weather accepted an unknown city query into saved locations")
    desktop.keys("esc")
    require(desktop.guest("web", "state", app=actor["id"])["settings"]["locations"] == negative_state["settings"]["locations"], "unmatched Weather city search changed actual saved cities")
    evidence["negative_search"] = desktop.observe("weather-no-fabricated-city", actor)
    city_button = control(desktop, actor, ("Search for a city or country", "New York", "Paris", "Places"), ("push button", "button", "toggle button"))
    desktop.click_path(actor, city_button["path"])
    replace_field(desktop, actor, "New York")
    desktop.keys("ctrl", "a")
    desktop.keys("ctrl", "c")
    evidence["city_search_clipboard"] = clipboard_text(desktop, "New York")
    result = desktop.await_state("weather-installed-real-city-index", lambda: desktop.tree(actor),
                                lambda row: any(n.get("role") in ("list item", "row") and "New York" in n.get("name", "") for n in nodes(row)), timeout=30)
    city = unique(result, lambda n: n.get("role") in ("list item", "row") and "New York" in n.get("name", ""), "real indexed New York city")
    desktop.click_path(actor, city["path"])
    content(desktop, actor, "weather-real-provider-content", "New York", timeout=30)
    evidence["provider"] = desktop.guest("web", "forecast", location="New York")
    provider = evidence["provider"]
    require("New York" in provider["location"], "actual saved weather location differs from chosen city")
    press(desktop, actor, "Main Menu")
    press(desktop, actor, "Celsius")
    desktop.keys("esc")
    press(desktop, actor, "Daily Forecast")
    evidence["forecast_ui"] = desktop.await_state("weather-dated-actual-forecast", lambda: desktop.observe("weather-dated-actual-forecast", actor),
                                                lambda row: bool(_weather_values(row)) and "Forecast not Available" not in words(row), timeout=30)
    forecast_temperatures = [x["celsius"] for x in provider["forecast"]]
    require(any(abs(actual - expected) <= 2 for actual in _weather_values(evidence["forecast_ui"]) for expected in forecast_temperatures), "actual rendered forecast differs from independent real provider values")
    rendered_forecast = " ".join(words(evidence["forecast_ui"]).split())
    require(any(" ".join(item["local_date"].split()) in rendered_forecast for item in provider["forecast"] if item["unix"] > provider["observed_unix"]),
            "actual Weather forecast does not render any independently dated future provider content")
    saved = desktop.guest("web", "state", app=actor["id"])

    def principal(current, stage):
        press(desktop, current, "Daily Forecast")
        row = desktop.await_state("weather-content-" + stage, lambda: desktop.observe("weather-content-" + stage, current),
                                  lambda value: "New York" in words(value) and bool(_weather_values(value)) and "Forecast not Available" not in words(value), timeout=30)
        require(any(abs(actual - expected) <= 2 for actual in _weather_values(row) for expected in forecast_temperatures), "Weather lost actual network forecast content")
        require(any(" ".join(item["local_date"].split()) in " ".join(words(row).split()) for item in provider["forecast"] if item["unix"] > provider["observed_unix"]),
                "Weather principal lost independently dated future forecast content")
        actual = desktop.guest("web", "state", app=current["id"])
        require(actual["settings"]["locations"] == saved["settings"]["locations"], "Weather policy transition changed real saved-city payload")
        # Weather intentionally has no error toast for GeoClue errors: its
        # currentLocationController logs the error, leaves manual cities usable.
        # Independent real client/service state above distinguishes deny/outage.
        service = None if stage in ("permission-denied", "master-disabled", "unavailable-service") else _active_fix(desktop, current)
        return {"rendered_manual_forecast": row, "saved_city_readback": actual, "actual_native_location_callback": service,
                "location_error_semantics": "manual-saved-city forecast remains usable; no invented GeoClue-error UI"}

    actor, evidence["location_paths"] = _policy_paths(desktop, actor, "Weather", settings, principal)
    evidence["crosscut"] = desktop.crosscut(actor, principal)
    return desktop.result(actor, "real-network-numerical-dated-forecast-city-search-geoclue-policy-recovery", evidence,
                          installed_signed_release_qualified=False, physical_geolocation_qualified=False)


def _elapsed(observation):
    parts = []
    for node in nodes(observation):
        if node.get("role") not in ("label", "static", "text"):
            continue
        text = node.get("text") or node.get("name") or ""
        parts.append(re.sub(r"[\s\u200e\u200f\u2066-\u2069]", "", text))
    combined = "".join(parts)
    matches = re.findall(r"(?<!\d)(\d{2})[∶:](\d{2})[∶:](\d{2})(?:\.(\d))?", combined)
    require(len(matches) == 1, "cannot uniquely read actual native elapsed/countdown content")
    hours, minutes, seconds, tenths = matches[0]
    return int(hours) * 3600 + int(minutes) * 60 + int(seconds) + (int(tenths) / 10 if tenths else 0)


def _world_clock(desktop, actor, stage):
    observed = desktop.await_state("clocks-world-" + stage,
        lambda: {"ui": content(desktop, actor, "clocks-world-" + stage, "Tokyo"),
                 "state": desktop.guest("web", "state", app=actor["id"])},
        lambda value: value["state"]["tokyo_time"][value["state"]["clock_format"]] in
            re.sub(r"[\s\u200e\u200f\u2066-\u2069]", "", words(value["ui"])).replace("∶", ":"))
    return {"actual_world_ui": observed["ui"], "independent_time_and_config": observed["state"]}


def _copy_help_page(desktop, actor, title, paragraph):
    row = desktop.tree(actor)
    target = unique(row, lambda n: n.get("role") == "paragraph" and
                    " ".join((n.get("text") or n.get("name", "")).split()) == paragraph,
                    "actual installed Help paragraph")
    desktop.click_path(actor, target["path"])
    desktop.keys("ctrl", "a")
    desktop.keys("ctrl", "c")
    copied = desktop.clipboard()
    text = base64.b64decode(copied["base64"], validate=True).decode("utf-8")
    require(title in text and paragraph in " ".join(text.split()), "actual Help selection clipboard lacks installed rendered page payload")
    return copied


def _alarm_time(desktop, actor, hour, minute, clock_format):
    require(clock_format in ("12h", "24h"), "unknown actual native clock format")
    replace_field(desktop, actor, str(hour if clock_format == "24h" else (hour % 12 or 12)), "Hours")
    replace_field(desktop, actor, str(minute), "Minutes")
    if clock_format == "12h":
        expected = "PM" if hour >= 12 else "AM"
        current = control(desktop, actor, ("AM", "PM"), ("push button", "button"))
        if current["name"] != expected:
            desktop.press(actor, current["name"], role=current["role"])
        desktop.await_state("actual-native-alarm-period", lambda: desktop.tree(actor),
                            lambda row: any(n.get("name") == expected and n.get("role") in ("push button", "button") for n in nodes(row)))


def _clocks(desktop):
    actor = desktop.start("org.gnome.clocks", "Clocks")
    evidence = {}
    initial_clock = desktop.guest("web", "state", app=actor["id"])
    alarm_hour = initial_clock["alarm_fixture_time"]["hour"]
    alarm_minute = initial_clock["alarm_fixture_time"]["minute"]
    evidence["future_alarm_payload_control"] = initial_clock
    press(desktop, actor, "Stopwatch")
    require(_elapsed(desktop.tree(actor)) == 0, "new native stopwatch unexpectedly nonzero")
    press(desktop, actor, "Start")
    started = time.monotonic()
    desktop.await_state("stopwatch-real-elapsed", lambda: desktop.tree(actor), lambda row: _elapsed(row) >= 2, timeout=10)
    press(desktop, actor, "Pause")
    elapsed = _elapsed(desktop.tree(actor))
    require(abs(elapsed - (time.monotonic() - started)) < 2, "native stopwatch elapsed differs from real monotonic elapsed")
    time.sleep(1.2)
    evidence["paused_negative"] = desktop.observe("stopwatch-pause-unchanged", actor)
    require(_elapsed(evidence["paused_negative"]) == elapsed, "paused native stopwatch advanced")
    press(desktop, actor, "Resume")
    desktop.await_state("stopwatch-actual-resume", lambda: desktop.tree(actor), lambda row: _elapsed(row) >= elapsed + 1)
    press(desktop, actor, "Pause")
    press(desktop, actor, "Clear")
    require(_elapsed(desktop.tree(actor)) == 0, "actual Stopwatch Clear did not reset elapsed")
    press(desktop, actor, "Timer")
    replace_field(desktop, actor, "30", "Seconds")
    replace_field(desktop, actor, "core-timer-" + desktop.nonce, "Title")
    press(desktop, actor, "Start")
    desktop.await_state("timer-real-countdown", lambda: desktop.tree(actor), lambda row: 20 <= _elapsed(row) <= 28)
    press(desktop, actor, "Pause")
    paused = _elapsed(desktop.tree(actor))
    time.sleep(1.2)
    evidence["timer_paused"] = desktop.observe("timer-pause-unchanged", actor)
    require(_elapsed(evidence["timer_paused"]) == paused, "paused actual timer decremented")
    press(desktop, actor, "World")
    desktop.keys("ctrl", "n")
    replace_field(desktop, actor, "Tokyo", "Search cities")
    search = desktop.await_state("clocks-real-world-city", lambda: desktop.tree(actor),
                                 lambda row: "Tokyo" in words(row), timeout=30)
    tokyo = unique(search, lambda n: n.get("role") in ("row", "list item") and "Tokyo" in n.get("name", ""), "indexed Tokyo world clock")
    desktop.click_path(actor, tokyo["path"])
    press(desktop, actor, "Add")
    content(desktop, actor, "clocks-world-ui", "Tokyo")
    press(desktop, actor, "Alarms")
    desktop.keys("ctrl", "n")
    name = field(desktop, actor, "Name")
    evidence["ime"] = desktop.ime(actor, name["name"], field_path=name["path"])
    alarm_name = "core-alarm-" + desktop.nonce
    replace_field(desktop, actor, alarm_name, "Name")
    _alarm_time(desktop, actor, alarm_hour, alarm_minute, initial_clock["clock_format"])
    press(desktop, actor, "Cancel")
    negative = desktop.guest("web", "state", app=actor["id"])
    require(not any(a.get("name") == alarm_name for a in negative["values"]["alarms"]), "cancelled native alarm was persisted")
    evidence["alarm_cancel"] = negative
    desktop.keys("ctrl", "n")
    replace_field(desktop, actor, alarm_name, "Name")
    _alarm_time(desktop, actor, alarm_hour, alarm_minute, initial_clock["clock_format"])
    press(desktop, actor, "Add")
    content(desktop, actor, "clocks-real-alarm-config", alarm_name)
    saved = desktop.guest("web", "state", app=actor["id"])
    alarm = [a for a in saved["values"]["alarms"] if a.get("name") == alarm_name]
    require(len(alarm) == 1 and alarm[0]["hour"] == alarm_hour and alarm[0]["minute"] == alarm_minute, "native alarm exact configuration readback differs")
    require(any(l["name"] == "Tokyo" and l["timezone"] == "Asia/Tokyo" for l in saved["world_locations"]), "actual world-clock city/timezone not persisted")
    require(any(t.get("name") == "core-timer-" + desktop.nonce and t.get("duration") == 30 for t in saved["values"]["timers"]), "native timer exact duration/title not persisted")

    def probe(current, stage):
        row = content(desktop, current, "clocks-alarm-" + stage, alarm_name)
        state = desktop.guest("web", "state", app=current["id"])
        require(state["values"]["alarms"] == saved["values"]["alarms"] and state["world_locations"] == saved["world_locations"], "Clocks actual saved world/alarm payload changed")
        press(desktop, current, "World")
        world = _world_clock(desktop, current, stage)
        press(desktop, current, "Alarms")
        return {"actual_alarm_ui": row, "config_readback": state, "world_clock": world}

    def restarted(current, stage):
        press(desktop, current, "Alarms")
        first = probe(current, stage)
        press(desktop, current, "World")
        first["world_ui"] = _world_clock(desktop, current, "after-restart")
        press(desktop, current, "Timer")
        first["timer_ui"] = desktop.observe("clocks-restarted-timer", current)
        require(_elapsed(first["timer_ui"]) == 30, "timer transient elapsed did not reset to persisted configured duration on restart")
        press(desktop, current, "Stopwatch")
        require(_elapsed(desktop.tree(current)) == 0, "stopwatch transient elapsed survived restart unexpectedly")
        press(desktop, current, "Alarms")
        return first

    evidence["stored"] = saved
    evidence["crosscut"] = desktop.crosscut(actor, probe, restart_probe=restarted)
    return desktop.result(actor, "monotonic-stopwatch-countdown-pause-reset-persisted-world-alarm", evidence,
                          installed_signed_release_qualified=False)


TOUR_PAGES = (
    ("Let's Begin", "Learn about the key features"),
    ("Get an Overview", "The overview shows all your apps and windows."),
    ("Powerful Search", "To search, just start typing in the overview."),
    ("Stay Organized With Workspaces", "Organize your windows"),
    ("Swipe Up and Down", "swipe up with three fingers"),
    ("Swipe Left and Right", "swipe three fingers horizontally"),
    ("That's It!", "see the Help app."),
)


def _tour(desktop):
    actor = desktop.start("org.gnome.Tour", "Tour")
    evidence = {"pages": []}
    content(desktop, actor, "tour-actual-intro", *TOUR_PAGES[0])
    press(desktop, actor, "Start")
    evidence["pages"].append(content(desktop, actor, "tour-first-content", *TOUR_PAGES[1]))
    desktop.keys("right")
    evidence["keyboard_next"] = content(desktop, actor, "tour-actual-keyboard-next", *TOUR_PAGES[2])
    desktop.keys("left")
    evidence["keyboard_back"] = content(desktop, actor, "tour-actual-keyboard-back", *TOUR_PAGES[1])
    press(desktop, actor, "Previous")
    evidence["reset"] = content(desktop, actor, "tour-reset-to-intro", *TOUR_PAGES[0])
    desktop.keys("left")
    evidence["negative_intro_boundary"] = content(desktop, actor, "tour-previous-at-first-unchanged", *TOUR_PAGES[0])
    press(desktop, actor, "Start")
    for number in range(2, len(TOUR_PAGES)):
        desktop.keys("right")
        evidence["pages"].append(content(desktop, actor, "tour-page-" + str(number), *TOUR_PAGES[number]))
    before = desktop.guest("web", "state", app=actor["id"])
    tree = desktop.tree(actor)
    interface_observations = [tree, evidence["keyboard_next"], evidence["keyboard_back"],
                              evidence["reset"], evidence["negative_intro_boundary"], *evidence["pages"]]
    require(not any(n.get("editable") for observed in interface_observations for n in nodes(observed)),
            "Tour exposes editable input; original-app IME is required")
    require(not any(n.get("role") in ("link", "menu item", "entry", "search box") for n in nodes(tree)),
            "Tour exposes an applicable handler/clipboard/input interface not exercised by the carousel sequence")
    require(not any("Tour" in schema or "tour" in schema for schema in before["installed_schemas"]), "installed Tour exposes preferences not exercised by this actual interface")
    evidence["input_applicability"] = {"status": "not-applicable", "verified_no_editable_field": True,
        "observed_tree": tree, "all_page_observations": interface_observations,
        "source_contracts": [
            "https://github.com/GNOME/gnome-tour/blob/c95566561a741f08158069676f9aa5707aa07cc6/data/resources/ui/window.ui",
            "https://github.com/GNOME/gnome-tour/blob/c95566561a741f08158069676f9aa5707aa07cc6/data/resources/ui/image-page.ui",
            "https://github.com/GNOME/gnome-tour/blob/c95566561a741f08158069676f9aa5707aa07cc6/src/widgets/paginator.rs"],
        "reason": "actual Tour interface has a carousel and Previous/Next/Start only, no editable field or copyable text; real Left/Right keyboard focus navigation exercised",
        "persistence": "carousel intentionally resets on new native process; installed Tour has no GSettings preferences schema; actual per-app files observed before/after"}
    # Leave a noninitial real page so restart reset cannot pass accidentally.
    desktop.keys("left")
    content(desktop, actor, "tour-noninitial-before-crosscut", *TOUR_PAGES[5])

    def probe(current, stage):
        row = content(desktop, current, "tour-" + stage, *TOUR_PAGES[5])
        return {"actual_content": row, "persistent_files": desktop.guest("web", "state", app=current["id"])}

    def restarted(current, stage):
        row = content(desktop, current, "tour-" + stage, *TOUR_PAGES[0])
        state = desktop.guest("web", "state", app=current["id"])
        require(state["persistent_files"] == before["persistent_files"], "Tour wrote unaccounted persisted state; observed reset semantics incomplete")
        press(desktop, current, "Start")
        return {"actual_reset_content": row, "post_restart_navigation": content(desktop, current, "tour-after-restart-consumer", *TOUR_PAGES[1]), "state": state}

    evidence["crosscut"] = desktop.crosscut(actor, probe, restart_probe=restarted)
    return desktop.result(actor, "installed-tour-content-all-pages-keyboard-back-reset-and-observed-restart-semantics", evidence,
                          installed_signed_release_qualified=False, ime_applicability=evidence["input_applicability"])


def _help(desktop, endpoint):
    installed = desktop.guest("web", "help-content")
    actor = desktop.start("org.gnome.Yelp", "Help")
    evidence = {"installed_content": installed}
    content(desktop, actor, "help-installed-user-docs-index", installed["pages"][0]["title"], timeout=30)
    desktop.keys("ctrl", "s")
    search = field(desktop, actor)
    evidence["ime"] = desktop.ime(actor, search["name"], field_path=search["path"])
    replace_field(desktop, actor, "tuna-no-installed-help-" + desktop.nonce)
    desktop.keys("ret")
    evidence["negative_search"] = content(desktop, actor, "help-no-real-search-result", "No matching help pages found", timeout=30)
    desktop.keys("ctrl", "s")
    replace_field(desktop, actor, "keyboard")
    desktop.keys("ret")
    expected = installed["pages"][2]
    content(desktop, actor, "help-real-installed-search", expected["title"], timeout=30)
    press(desktop, actor, expected["title"], ("link",))
    paragraph = next(p for p in expected["paragraphs"] if len(p) >= 35)
    evidence["rendered"] = content(desktop, actor, "help-rendered-installed-mallard", expected["title"], paragraph, timeout=30)
    evidence["rendered_clipboard"] = _copy_help_page(desktop, actor, expected["title"], paragraph)
    desktop.keys("alt", "left")
    evidence["back"] = content(desktop, actor, "help-real-search-back", expected["title"])
    desktop.keys("alt", "right")
    content(desktop, actor, "help-real-content-forward", paragraph)
    press(desktop, actor, "Bookmarks")
    press(desktop, actor, "Add Bookmark")
    desktop.keys("esc")
    before_zoom = desktop.guest("web", "state", app=actor["id"])
    desktop.keys("ctrl", "shift", "equal")
    persisted = desktop.await_state("help-actual-text-zoom-setting",
        lambda: desktop.guest("web", "state", app=actor["id"]),
        lambda row: row["settings"]["zoom-level"] != before_zoom["settings"]["zoom-level"])
    require(any("keyboard-shortcuts-set" in section.get("bookmarks", "") for section in persisted["document_settings"].values()),
            "actual Help bookmark was not written to real per-document settings")
    evidence["zoom_persisted"] = persisted
    # A genuine browser link invokes the installed help: URI handler. The
    # browser only supplies content; it does not spoof Yelp's resulting page.
    browser = desktop.start("org.gnome.Epiphany", "Web")
    _navigate(desktop, browser, endpoint["url"])
    content(desktop, browser, "help-handler-browser-source", "Browser content TUNA-WEB-" + desktop.nonce)
    press(desktop, browser, "Open installed Help", ("link",))
    content(desktop, browser, "help-external-handler-consent", "Open in External App?")
    press(desktop, browser, "Open Link")
    desktop.select(actor)
    evidence["handler"] = content(desktop, actor, "help-actual-uri-handler-rendered", expected["title"], paragraph, timeout=30)
    desktop.close(browser)
    desktop.select(actor)

    def probe(current, stage):
        row = content(desktop, current, "help-" + stage, expected["title"], paragraph, timeout=30)
        saved = desktop.guest("web", "state", app=current["id"])
        require(saved["settings"] == persisted["settings"], "actual Help text zoom preference did not persist")
        return {"rendered_installed_paragraph": row, "readback": saved,
                "actual_rendered_clipboard": _copy_help_page(desktop, current, expected["title"], paragraph)}

    def restarted(current, stage):
        press(desktop, current, "Bookmarks")
        press(desktop, current, expected["title"])
        return probe(current, stage)

    evidence["crosscut"] = desktop.crosscut(actor, probe, restart_probe=restarted)
    return desktop.result(actor, "installed-gnome-user-docs-search-rendered-paragraph-link-history-bookmark-handler", evidence,
                          installed_signed_release_qualified=False)


def run(desktop, *, prepared=None, endpoint=None):
    require(desktop.installed_signed_release_qualified is False, "DEV core-app evidence must not become signed-release qualification")
    receipts = []
    require((prepared is None) == (endpoint is None), "shared endpoint preparation/identity must be supplied together")
    owns_endpoint = prepared is None
    permissions = desktop.guest("web", "permission-save")
    desktop.retain("web-prelaunch-location-permissions", permissions)
    endpoint_preparation_attempted = False
    watching = None
    try:
        watching = desktop.guest("web", "location-watch-start")
        desktop.await_state("real-original-consumer-location-observer-ready",
                            lambda: desktop.guest("web", "location-watch-ready"), lambda row: row.get("ready"))
        if owns_endpoint:
            endpoint_preparation_attempted = True
            prepared = desktop.guest("web", "prepare")
            endpoint = desktop.await_state("ordinary-owned-http-content-ready", lambda: desktop.guest("web", "endpoint"), lambda value: value.get("ready"))
        else:
            current_endpoint = desktop.guest("web", "endpoint")
            require(all(current_endpoint.get(key) == endpoint.get(key)
                        for key in ("pid", "uid", "nonce", "start_ticks", "url")),
                    "original shared HTTP content endpoint changed before browser consumer")
        receipts.append(_browser(desktop, prepared, endpoint))
        receipts.append(_characters(desktop))
        # Source ownership is shared with autoTZ. Parent owns one install/finally
        # restore lifecycle around both domains, never a duplicate provider.
        settings = _location_settings(desktop)
        previous_master = desktop.guest("web", "state", app="org.gnome.Maps")["location_master"]
        try:
            _master(desktop, settings, True, "org.gnome.Maps")
            receipts.append(_maps(desktop, settings, endpoint))
            receipts.append(_weather(desktop, settings))
        finally:
            _master(desktop, settings, previous_master, "org.gnome.Maps")
            desktop.close(settings)
        receipts.append(_clocks(desktop))
        receipts.append(_tour(desktop))
        receipts.append(_help(desktop, endpoint))
    finally:
        try:
            if endpoint_preparation_attempted:
                desktop.guest("web", "stop-endpoint")
        finally:
            try:
                if watching is not None:
                    desktop.retain("web-passive-location-observer-retirement", desktop.guest("web", "location-watch-stop"))
            finally:
                cleanup = desktop.guest("web", "permission-restore")
                desktop.retain("web-owned-location-permission-cleanup", cleanup)
    return receipts
