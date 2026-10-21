#!/usr/bin/env python3
"""Synthetic mounted trees and pure evidence tests; no image, PAM or DM runs."""
import contextlib
import copy
import io
import json
import os
from pathlib import Path
import stat
import tempfile
import unittest
from unittest.mock import patch

import published_dm_contract as dm


ENTRY = ("[Desktop Entry]\nName=Tuna Desktop\nExec=tuna-session\nTryExec=tuna-session\n"
         "Type=Application\nDesktopNames=Tuna;GNOME;\nX-GDM-SessionRegisters=true\n")
UNIT = "[Unit]\nAfter=one.service\nAfter=two.service\n[Service]\nExecStart=/usr/bin/gdm\n"
SCHEMAS = "<gdmschemafile><schemalist>" + "".join(
    f"<schema><key>{key}</key><signature>b</signature><default>false</default></schema>"
    for key in dm.DISABLED_LOGIN_DEFAULTS) + "</schemalist></gdmschemafile>"
SUBJECT = {"image": "ghcr.io/tuna-os/marlin@sha256:" + "a" * 64,
           "platform_digest": "sha256:" + "b" * 64}


class SessionContracts(unittest.TestCase):
    def setUp(self):
        self.session = {"Id": "7", "User": "1000", "Service": "gdm-password", "Class": "user",
                        "Type": "wayland", "Seat": "seat0", "Remote": "no", "VTNr": "2",
                        "Active": "yes", "State": "active"}
        self.process = {"pid": 200, "start_ticks": 300, "uid": 1000,
                        "executable": "/usr/bin/tuna-compositor", "logind_session": "7"}

    def validate(self, name="published-gdm", **kwargs):
        return dm.validate_session(name, self.session, self.process, 1000, **kwargs)

    def test_gdm_uses_verified_dynamic_vt(self):
        for vt in ("1", "2", "9"):
            self.session["VTNr"] = vt
            self.assertEqual(self.validate()["vt"], int(vt))

    def test_current_greetd_remains_distinct_and_fixed_vt(self):
        self.session.update(Service="greetd", Type="tty", VTNr="1")
        self.assertEqual(self.validate("current-greetd")["profile"], "current-greetd")
        with self.assertRaises(dm.ContractError):
            self.validate()
        self.session["VTNr"] = "2"
        with self.assertRaises(dm.ContractError):
            self.validate("current-greetd")

    def test_unknown_or_inferred_profile_fails(self):
        for name in (None, "", "auto", "gdm", {}, "greetd"):
            with self.subTest(name=name), self.assertRaises(dm.ContractError):
                self.validate(name)

    def test_wrong_session_fields_fail(self):
        cases = {"User": ["0", "1001", 1000, None],
                 "Service": ["gdm-autologin", "gdm-launch-environment", "greetd", "runuser", None],
                 "Class": ["greeter", "manager", "manager-early", None],
                 "Type": ["tty", "x11", None], "Seat": ["seat1", "", None],
                 "Remote": ["yes", None], "Id": ["", "../7", 7, None],
                 "VTNr": ["0", "-1", "", "01", "2.0", 2, None],
                 "Active": ["no", None], "State": ["closing", "opening", None]}
        for field, values in cases.items():
            original = self.session[field]
            for value in values:
                self.session[field] = value
                with self.subTest(field=field, value=value), self.assertRaises(dm.ContractError):
                    self.validate()
            self.session[field] = original

    def test_process_identity_cannot_be_missing_or_wrong(self):
        cases = {"pid": [0, -1, True, "200", None], "start_ticks": [0, True, None],
                 "uid": [0, 1001, True, "1000", None], "logind_session": ["8", None],
                 "executable": ["tuna-compositor", "/tmp/tuna-compositor", "/usr/bin/gnome-shell", None]}
        for field, values in cases.items():
            original = self.process[field]
            for value in values:
                self.process[field] = value
                with self.subTest(field=field, value=value), self.assertRaises(dm.ContractError):
                    self.validate()
            self.process[field] = original

    def test_intentional_vt_pause_requires_explicit_policy(self):
        self.session.update(Active="no", State="online")
        with self.assertRaises(dm.ContractError):
            self.validate()
        self.assertEqual(self.validate(require_active=False)["vt"], 2)
        self.session["State"] = "closing"
        with self.assertRaises(dm.ContractError):
            self.validate(require_active=False)

    def test_root_uid_and_malformed_policy_refused(self):
        for uid in (0, -1, True, "1000", None):
            with self.subTest(uid=uid), self.assertRaises(dm.ContractError):
                dm.validate_session("published-gdm", self.session, self.process, uid)
        with self.assertRaises(dm.ContractError):
            self.validate(require_active="false")

    def test_new_process_is_valid_observation_but_different_continuity_identity(self):
        baseline = self.validate()
        self.process.update(pid=201, start_ticks=301)
        self.assertNotEqual(baseline, self.validate())


class StaticPolicy(unittest.TestCase):
    def test_default_and_explicitly_disabled_gdm_pass(self):
        dm.validate_gdm_config("# default\n[daemon]\n[security]\n")
        for false in ("false", "FALSE", "0"):
            dm.validate_gdm_config("[Daemon]\n AutomaticLoginEnable = " + false + "\n")

    def test_enabled_and_ambiguous_login_fail(self):
        for key in ("AutomaticLoginEnable", "TimedLoginEnable"):
            for value in ("true", "TRUE", "1", "yes", "on", "no", "off", "", "maybe"):
                with self.subTest(key=key, value=value), self.assertRaises(dm.ContractError):
                    dm.validate_gdm_config(f"[daemon]\n{key}={value}\n")

    def test_installed_login_defaults_are_required_and_disabled(self):
        self.assertEqual(dm.validate_gdm_defaults(SCHEMAS), dm.DISABLED_LOGIN_DEFAULTS)
        for text in (SCHEMAS.replace("<default>false", "<default>true", 1),
                     SCHEMAS.replace("AutomaticLoginEnable", "Missing"),
                     SCHEMAS.replace("TimedLoginEnable", "AutomaticLoginEnable"),
                     SCHEMAS.replace("<signature>b", "<signature>s", 1),
                     SCHEMAS.replace("<schemalist>", "<schema><key>daemon/AutomaticLoginEnable</key>"
                                     "<signature>b</signature><default>true</default></schema><schemalist>", 1),
                     SCHEMAS.replace("<default>false</default>", "<default><nested>false</nested></default>", 1),
                     SCHEMAS.replace("<signature>b</signature>", "<signature>b</signature><unknown/>", 1),
                     SCHEMAS.replace("<default>false</default>", "<default>fal<!--x-->se</default>", 1),
                     SCHEMAS.replace("<default>false</default>", "<default><![CDATA[false]]></default>", 1),
                     SCHEMAS.replace("<default>false</default>", "<default>fa&#108;se</default>", 1),
                     "<!DOCTYPE gdmschemafile>" + SCHEMAS, "<invalid/>"):
            with self.subTest(text=text), self.assertRaises(dm.ContractError):
                dm.validate_gdm_defaults(text)

    def test_duplicate_or_malformed_config_fails(self):
        for text in ("bad", "[daemon]\nAutomaticLoginEnable=false\nAutomaticLoginEnable=true\n",
                     "[daemon]\n[Daemon]\n", "[daemon]\n[ daemon ]\n"):
            with self.subTest(text=text), self.assertRaises(dm.ContractError):
                dm.validate_gdm_config(text)

    def test_indentation_does_not_hide_effective_glib_keys(self):
        with self.assertRaises(dm.ContractError):
            dm.validate_gdm_config("[daemon]\nUnrelated=value\n AutomaticLoginEnable=true\n")
        for added in (" Exec=/usr/bin/false\n", " Hidden=true\n"):
            with self.subTest(added=added), self.assertRaises(dm.ContractError):
                dm.validate_tuna_entry(ENTRY + "Comment=value\n" + added)

    def test_actual_session_entry_passes(self):
        dm.validate_tuna_entry(ENTRY)

    def test_hidden_modified_or_restricted_entry_fails(self):
        for text in (ENTRY + "Hidden=true\n", ENTRY + "NoDisplay=true\n",
                     ENTRY + "OnlyShowIn=GNOME;\n", ENTRY.replace("Exec=tuna-session", "Exec=wrapper"),
                     ENTRY.replace("Name=Tuna Desktop", "Name=tuna-session"),
                     ENTRY.replace("TryExec=tuna-session\n", ""), ENTRY + "Exec=tuna-session\n"):
            with self.subTest(text=text), self.assertRaises(dm.ContractError):
                dm.validate_tuna_entry(text)

    def test_desktop_entry_group_key_case_and_equals_are_exact(self):
        for text in (ENTRY.replace("Desktop Entry", "desktop entry"),
                     ENTRY.replace("Exec=", "exec="), ENTRY.replace("Exec=", "Exec:"),
                     "[DEFAULT]\nName=Tuna Desktop\n" + ENTRY):
            with self.subTest(text=text), self.assertRaises(dm.ContractError):
                dm.validate_tuna_entry(text)

    def test_unit_allows_repeated_ordering_but_no_launch_substitution(self):
        dm.validate_gdm_unit(UNIT)
        dm.validate_gdm_unit(UNIT + "EnvironmentFile=-/etc/locale.conf\n")
        for text in (UNIT.replace("/usr/bin/gdm", "/tmp/gdm"), UNIT + "ExecStart=/usr/bin/gdm\n",
                     UNIT + "ExecStartPre=/bin/true\n", UNIT + "Environment=LD_PRELOAD=/tmp/a.so\n",
                     UNIT + "EnvironmentFile=/tmp/override\n", UNIT + "RootDirectory=/alternate\n",
                     UNIT + "BindPaths=/replacement:/usr/bin/gdm\n"):
            with self.subTest(text=text), self.assertRaises(dm.ContractError):
                dm.validate_gdm_unit(text)

    def test_pam_includes_bracket_control_and_optional_modules(self):
        includes, modules = dm.parse_pam("auth include system-local-login\n"
                                        "-session optional pam_oo7.so auto_start\n"
                                        "auth [success=ok default=1] pam_gdm.so\n")
        self.assertEqual(includes, ["system-local-login"])
        self.assertEqual(modules, [("pam_oo7.so", True), ("pam_gdm.so", False)])
        self.assertEqual(dm.parse_pam("@include login\n")[0], ["login"])

    def test_malformed_or_escaping_pam_fails(self):
        for text in ("", "auth include ../login", "auth required ./module.so",
                     "auth required /tmp/module.so", "auth required /usr/lib/security/../a.so",
                     "@include login extra", "auth [broken pam_unix.so",
                     "auth invented pam_unix.so", "auth [wat=ok] pam_unix.so",
                     "auth [success=invented] pam_unix.so", "auth [] pam_unix.so"):
            with self.subTest(text=text), self.assertRaises(dm.ContractError):
                dm.parse_pam(text)

    def test_leading_dash_does_not_make_required_pam_modules_optional(self):
        for control in ("required", "requisite", "sufficient", "[default=bad]"):
            self.assertEqual(dm.parse_pam(f"-auth {control} pam_missing.so")[1],
                             [("pam_missing.so", False)])


class FixtureInventory(dm.Inventory):
    """Test files belong to the runner; model image UID/GID without chown."""
    def record(self, name):
        result = super().record(name)
        if result["type"] != "missing":
            result.update(uid=0, gid=0)
        return result


class MountedInventory(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.root.chmod(0o755)  # Model traversable image / without chown.
        self.owners = dict(dm.REQUIRED_OWNERS)
        self.packages = dict.fromkeys(("gdm", "tuna-desktop", "gnome-shell", "pam"), "1.0-1")
        self.queries = []
        for name in dm.REQUIRED_OWNERS:
            self.write(name, "synthetic package payload\n", executable=name in dm.TUNA_BINS or
                       name in ("/usr/bin/gdm", "/usr/bin/gnome-shell", "/usr/lib/gdm-session-worker",
                                "/usr/lib/gdm-wayland-session"))
        self.write("/etc/gdm/custom.conf", "[daemon]\n[security]\n")
        self.write("/usr/share/gdm/gdm.schemas", SCHEMAS)
        self.write(dm.GDM_UNIT, UNIT)
        self.write("/usr/share/wayland-sessions/tuna.desktop", ENTRY)
        for name in dm.DM_LINKS[:2]:
            self.link(name, dm.GDM_UNIT)
        for service, owner in (("gdm-password", "gdm"), ("gdm-launch-environment", "gdm"),
                               ("tuna-lock", "tuna-desktop")):
            name = ("/etc/pam.d/" if service == "tuna-lock" else "/usr/lib/pam.d/") + service
            self.write(name, "auth include system-local-login\n-session optional pam_missing.so\n")
            self.owners[name] = owner
        self.write("/usr/lib/pam.d/system-local-login", "auth required pam_unix.so\n")
        self.owners["/usr/lib/pam.d/system-local-login"] = "pam"
        self.write("/usr/lib/security/pam_unix.so", "synthetic PAM module\n")
        self.owners["/usr/lib/security/pam_unix.so"] = "pam"

    def write(self, name, text, executable=False):
        path = self.root / name.lstrip("/")
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(text)
        path.chmod(0o755 if executable else 0o644)
        return path

    def link(self, name, target):
        path = self.root / name.lstrip("/")
        path.parent.mkdir(parents=True, exist_ok=True)
        path.symlink_to(target)
        return path

    def query(self, arguments):
        self.queries.append(arguments)
        if arguments == ["-Q"]:
            return "".join(f"{name} {version}\n" for name, version in self.packages.items())
        if len(arguments) == 3 and arguments[:2] == ["-Qqo", "--"]:
            owner = self.owners.get(arguments[2])
            if owner is not None:
                return owner + "\n"
            raise dm.ContractError("no installed owner")
        raise AssertionError("unexpected non-read-only package command")

    def collect(self):
        result = FixtureInventory(self.root, self.query).collect()
        result["subject"] = dict(SUBJECT)
        return result

    def assert_refused(self, fragment=None):
        result = self.collect()
        self.assertFalse(result["static_inventory_ready"], result)
        self.assertFalse(result["runtime_qualified"])
        if fragment:
            self.assertIn(fragment, "\n".join(result["errors"]))
        return result

    def test_complete_inventory_is_static_only_and_read_only(self):
        before = sorted(str(path.relative_to(self.root)) for path in self.root.rglob("*"))
        report = self.collect()
        self.assertTrue(report["static_inventory_ready"], report["errors"])
        self.assertFalse(report["runtime_qualified"])
        self.assertFalse(report["effective_launch_verified"])
        self.assertIn("effective-gdm-session-search-path", report["unverified_checks"])
        self.assertEqual(report["evidence_kind"], "mounted-filesystem-preflight")
        self.assertEqual(report["pam"]["gdm-password"]["effective_path"], "/usr/lib/pam.d/gdm-password")
        self.assertEqual(report["files"]["/etc/systemd/system/display-manager.service"]["target"], dm.GDM_UNIT)
        self.assertEqual(before, sorted(str(path.relative_to(self.root)) for path in self.root.rglob("*")))
        self.assertTrue(all(args == ["-Q"] or args[:2] == ["-Qqo", "--"] for args in self.queries))

    def test_missing_required_package_and_legacy_provides_fail(self):
        del self.packages["tuna-desktop"]
        self.packages["roost"] = "1.0-1"
        self.assert_refused("required installed package")

    def test_wrong_file_owner_fails_even_if_package_present(self):
        self.owners["/usr/bin/gdm"] = "tuna-desktop"
        self.assert_refused("ownership differs: /usr/bin/gdm")

    def test_missing_executable_never_gets_filled_in(self):
        path = self.root / "usr/bin/gdm"
        path.unlink()
        self.assert_refused("required regular shipped file missing")
        self.assertFalse(path.exists())

    def test_non_executable_or_writable_binary_fails(self):
        path = self.root / "usr/bin/gdm"
        path.chmod(0o644)
        self.assert_refused("not executable")
        path.chmod(0o777)
        self.assert_refused("write permissions")
        path.chmod(0o700)
        self.assert_refused("not executable")
        path.chmod(0o701)
        self.assert_refused("not executable")

    def test_writable_parent_directory_fails(self):
        (self.root / "usr/bin").chmod(0o777)
        self.assert_refused("untrusted shipped parent permissions")

    def test_writable_manager_policy_directories_fail_collection_and_comparison(self):
        for name in ("/etc/systemd/system", "/etc/systemd/system/graphical.target.wants",
                     "/etc/systemd/system/gdm.service.d", "/usr/share/xsessions",
                     "/etc/gdm/nested"):
            path = self.root / name.lstrip("/")
            path.mkdir(exist_ok=True)
            before = self.collect()
            self.assertTrue(before["static_inventory_ready"], before["errors"])
            path.chmod(0o777)
            with self.subTest(name=name):
                self.assert_refused("untrusted shipped parent permissions")
                before["files"][name]["mode"] = 0o777
                with self.assertRaises(dm.ContractError):
                    dm.compare_immutable(before, copy.deepcopy(before))
            path.chmod(0o755)

    def test_other_protected_configuration_files_cannot_be_writable(self):
        path = self.write("/etc/gdm/extra.conf", "# extra protected configuration\n")
        before = self.collect()
        path.chmod(0o666)
        self.assert_refused("write permissions")
        before["files"]["/etc/gdm/extra.conf"]["mode"] = 0o666
        with self.assertRaises(dm.ContractError):
            dm.compare_immutable(before, copy.deepcopy(before))

    def test_chained_parent_symlinks_have_complete_link_evidence(self):
        (self.root / "payload").mkdir()
        (self.root / "usr/bin").rename(self.root / "payload/bin")
        self.link("/usr/bin", "/redirect/bin")
        self.link("/redirect/bin", "/payload/bin")
        report = self.collect()
        self.assertTrue(report["static_inventory_ready"], report["errors"])
        dm.compare_immutable(report, copy.deepcopy(report))
        self.assertIn("/redirect/bin", report["files"])

    def test_no_fixture_process_may_replace_owned_binary_with_link(self):
        path = self.root / "usr/bin/gdm"
        path.unlink()
        path.symlink_to("/usr/bin/gnome-shell")
        self.assert_refused("required regular shipped file")

    def test_residual_greetd_list_does_not_change_backend(self):
        self.write("/etc/greetd/environments", "Tuna Desktop\n")
        self.assertEqual(self.collect()["profile"], "published-gdm")
        self.assertTrue(self.collect()["static_inventory_ready"])

    def test_wrong_manager_link_and_competing_enablement_fail(self):
        self.link("/etc/systemd/system/graphical.target.wants/greetd.service", "/usr/lib/systemd/system/greetd.service")
        self.assert_refused("competing display manager")

    def test_competing_manager_cannot_hide_under_an_alias(self):
        self.link("/etc/systemd/system/graphical.target.wants/custom.service",
                  "/usr/lib/systemd/system/greetd.service")
        self.assert_refused("competing display manager")

    def test_extra_gdm_alias_is_rejected_before_alias_dropins(self):
        self.link("/etc/systemd/system/alternate-manager.service", dm.GDM_UNIT)
        self.write("/etc/systemd/system/alternate-manager.service.d/override.conf", "[Service]\n")
        self.assert_refused("additional GDM unit alias")

    def test_symlinked_enablement_directory_cannot_hide_competitors(self):
        self.link("/hidden-enablements/greetd.service", "/usr/lib/systemd/system/greetd.service")
        self.link("/etc/systemd/system/multi-user.target.wants", "/hidden-enablements")
        self.assert_refused("symlinked system enablement directory")

    def test_wrong_manager_target_fails(self):
        path = self.root / "etc/systemd/system/display-manager.service"
        path.unlink()
        path.symlink_to("/usr/lib/systemd/system/greetd.service")
        self.assert_refused("does not resolve to shipped GDM")

    def test_autologin_and_timed_login_fail(self):
        for key in ("AutomaticLoginEnable", "TimedLoginEnable"):
            self.write("/etc/gdm/custom.conf", f"[daemon]\n{key}=true\n")
            self.assert_refused("automatic or timed")

    def test_custom_config_omission_cannot_hide_enabled_schema_defaults(self):
        self.write("/usr/share/gdm/gdm.schemas", SCHEMAS.replace("<default>false", "<default>true", 1))
        self.assert_refused("schema login default")

    def test_known_alternate_session_entries_are_recorded_and_rejected(self):
        for root in dm.SESSION_SHADOW_ROOTS:
            path = self.write(root + "/tuna.desktop", ENTRY.replace("Exec=tuna-session", "Exec=/bin/false"))
            with self.subTest(root=root):
                report = self.assert_refused("alternate Tuna session entry")
                self.assertIn(root + "/tuna.desktop", report["files"])
            path.unlink()

    def test_pam_etc_override_is_effective_and_needs_package_owner(self):
        name = "/etc/pam.d/gdm-password"
        self.write(name, "auth required pam_unix.so\n")
        self.assert_refused("ownership differs: " + name)
        self.owners[name] = "gdm"
        report = self.collect()
        self.assertTrue(report["static_inventory_ready"], report["errors"])
        self.assertEqual(report["pam"]["gdm-password"]["effective_path"], name)

    def test_transitive_top_level_pam_still_checks_its_required_owner(self):
        self.write("/usr/lib/pam.d/gdm-password", "auth include tuna-lock\n")
        self.owners["/etc/pam.d/tuna-lock"] = "pam"
        self.assert_refused("ownership differs: /etc/pam.d/tuna-lock")

    def test_pam_missing_required_module_fails_but_optional_absence_is_recorded(self):
        report = self.collect()
        optional = report["pam"]["gdm-password"]["modules"][0]
        self.assertTrue(optional["optional_absence"])
        self.assertFalse(optional["present"])
        (self.root / "usr/lib/security/pam_unix.so").unlink()
        self.assert_refused("required regular shipped file missing")

    def test_missing_include_and_include_cycle_fail(self):
        self.write("/usr/lib/pam.d/system-local-login", "auth include missing\n")
        self.assert_refused("effective PAM service is missing")
        self.write("/usr/lib/pam.d/system-local-login", "auth include gdm-password\n")
        self.assert_refused("PAM include cycle")

    def test_any_gdm_dropin_requires_review(self):
        self.write("/etc/systemd/system/gdm.service.d/local.conf", "[Service]\nExecStart=/tmp/gdm\n")
        self.assert_refused("unreviewed GDM unit drop-in")

    def test_higher_priority_units_alias_dropins_and_generic_dropins_fail(self):
        for name in ("/run/systemd/system/gdm.service", "/usr/local/lib/systemd/system/gdm.service",
                     "/etc/systemd/system.control/gdm.service",
                     "/run/systemd/system.attached/gdm.service",
                     "/etc/systemd/system/display-manager.service.d/custom.conf",
                     "/usr/lib/systemd/system/service.d/custom.conf"):
            path = self.write(name, UNIT)
            with self.subTest(name=name):
                self.assert_refused("unreviewed")
            path.unlink()

    def test_symlinked_dropin_cannot_bypass_review(self):
        self.write("/review-dropins/local.conf", "[Service]\n")
        self.link("/etc/systemd/system/gdm.service.d", "/review-dropins")
        self.assert_refused("unreviewed GDM unit drop-in")

    def test_protected_tree_symlink_cycle_fails_bounded(self):
        self.link("/etc/gdm/loop", "/etc/gdm")
        self.assert_refused("tree link cycle")

    def test_runtime_override_prevents_pristine_preflight(self):
        self.write("/run/gdm/custom.conf", "[daemon]\n")
        self.assert_refused("runtime GDM configuration")

    def test_fifo_config_fails_without_waiting_for_a_writer(self):
        path = self.root / "etc/gdm/custom.conf"
        path.unlink()
        os.mkfifo(path)
        self.assert_refused("regular file")

    def test_locale_environment_is_recorded_and_only_allows_locale_variables(self):
        self.write("/etc/locale.conf", "LANG=en_US.UTF-8\n")
        before = self.collect()
        self.assertTrue(before["static_inventory_ready"], before["errors"])
        self.write("/etc/locale.conf", "LANG=C.UTF-8\n")
        with self.assertRaises(dm.ContractError):
            dm.compare_immutable(before, self.collect())
        self.write("/etc/locale.conf", "LD_PRELOAD=/tmp/inject.so\n")
        self.assert_refused("non-locale GDM environment setting")

    def test_writable_locale_environment_fails(self):
        path = self.write("/etc/locale.conf", "LANG=en_US.UTF-8\n")
        before = self.collect()
        path.chmod(0o666)
        self.assert_refused("write permissions")
        before["files"]["/etc/locale.conf"]["mode"] = 0o666
        with self.assertRaises(dm.ContractError):
            dm.compare_immutable(before, copy.deepcopy(before))

    def test_inventory_failure_preserves_other_observations(self):
        (self.root / "usr/bin/gdm").unlink()
        report = self.assert_refused()
        self.assertIn("/usr/share/wayland-sessions/tuna.desktop", report["files"])
        self.assertIn("gdm-password", report["pam"])

    def test_unchanged_inventory_and_additive_package_pass_comparison(self):
        before = self.collect()
        self.packages["qemu-guest-agent"] = "1.0-1"
        self.write("/usr/bin/qemu-ga", "additive executable\n", executable=True)
        self.write("/usr/lib/systemd/system/qemu-guest-agent.service", "[Service]\n")
        dm.compare_immutable(before, self.collect())

    def test_same_version_replaced_bytes_fail_comparison(self):
        before = self.collect()
        self.write("/usr/bin/gdm", "changed bytes at same package version\n", executable=True)
        after = self.collect()
        self.assertTrue(after["static_inventory_ready"], after["errors"])
        with self.assertRaisesRegex(dm.ContractError, "payload"):
            dm.compare_immutable(before, after)

    def test_new_non_autologin_config_member_still_fails_immutability(self):
        before = self.collect()
        self.write("/etc/gdm/new.conf", "# added configuration\n")
        after = self.collect()
        self.assertTrue(after["static_inventory_ready"], after["errors"])
        with self.assertRaisesRegex(dm.ContractError, "membership"):
            dm.compare_immutable(before, after)

    def test_package_removal_version_change_and_subject_change_fail(self):
        before = self.collect()
        for change in ("version", "removed", "subject"):
            after = copy.deepcopy(before)
            if change == "version":
                after["packages"]["gdm"] = "2.0-1"
            elif change == "removed":
                del after["packages"]["gdm"]
            else:
                after["subject"]["platform_digest"] = "sha256:" + "c" * 64
            with self.subTest(change=change), self.assertRaises(dm.ContractError):
                dm.compare_immutable(before, after)

    def test_missing_and_partial_evidence_never_passes_comparison(self):
        before = self.collect()
        mutations = [lambda d: d.update(evidence_kind="registry-layer-prefix"),
                     lambda d: d.update(static_inventory_ready=False),
                     lambda d: d.update(runtime_qualified=True),
                     lambda d: d.update(effective_launch_verified=True),
                     lambda d: d.update(unverified_checks=[]),
                     lambda d: d.update(gdm_login_defaults={}),
                     lambda d: d["gdm_login_defaults"].update({"daemon/AutomaticLoginEnable": 0}),
                     lambda d: d.pop("subject"), lambda d: d.update(errors=["failure"]),
                     lambda d: d["files"].pop("/usr/bin/gdm"),
                     lambda d: d["files"].pop("/etc/pam.d/tuna-lock"),
                     lambda d: d["pam"].pop("gdm-password"),
                     lambda d: d["pam"].pop("system-local-login"),
                     lambda d: d["files"]["/usr/bin/gdm"].update(type="missing"),
                     lambda d: d["files"]["/usr/bin/gdm"].pop("sha256"),
                     lambda d: d["files"]["/usr/bin/gdm"].update(mode=0o700),
                     lambda d: d["files"]["/usr/bin/tuna-compositor"].update(mode=0o644),
                     lambda d: d["files"]["/usr/lib/security/pam_unix.so"].update(uid=1000),
                     lambda d: d["files"]["/usr/lib/security"].update(mode=0o777),
                     lambda d: d["files"][dm.DM_LINKS[0]].update(target="/usr/lib/systemd/system/greetd.service"),
                     lambda d: d["files"]["/usr/bin"].update(mode=0o777),
                     lambda d: d["files"]["/etc/gdm"].pop("entries"),
                     lambda d: d["pam"]["gdm-password"]["modules"][0].update(path=[])]
        for mutation in mutations:
            damaged = copy.deepcopy(before)
            mutation(damaged)
            with self.subTest(mutation=mutation), self.assertRaises(dm.ContractError):
                dm.compare_immutable(damaged, copy.deepcopy(damaged))

    def test_nested_tree_members_and_link_targets_cannot_be_omitted(self):
        self.write("/etc/gdm/nested/setting", "# protected\n")
        self.write("/other-config/setting", "# protected\n")
        self.link("/etc/gdm/extra", "/other-config")
        before = self.collect()
        for mutation in (lambda d: d["files"]["/etc/gdm/nested"].pop("entries"),
                         lambda d: d["files"]["/etc/gdm/extra"].update(target="/unrecorded")):
            damaged = copy.deepcopy(before)
            mutation(damaged)
            with self.assertRaises(dm.ContractError):
                dm.compare_immutable(damaged, copy.deepcopy(damaged))


class RootSafety(unittest.TestCase):
    def test_absolute_symlink_stays_inside_mounted_root(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / "etc").mkdir()
            (root / "etc/passwd").write_text("synthetic fixture\n")
            (root / "alias").symlink_to("/etc/passwd")
            self.assertEqual(dm.Root(root).read_text("/alias"), "synthetic fixture\n")

    def test_escaping_link_and_symlink_cycle_fail(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / "escape").symlink_to("../../etc/passwd")
            (root / "cycle").symlink_to("/cycle")
            for name in ("/escape", "/cycle", "/../etc/passwd"):
                with self.subTest(name=name), self.assertRaises(dm.ContractError):
                    dm.Root(root).resolve(name)


class CommandBoundary(unittest.TestCase):
    def test_read_query_uses_absolute_pacman_and_no_shell(self):
        with patch.object(dm.subprocess, "run") as run:
            run.return_value.returncode = 0
            run.return_value.stdout = "gdm 1.0-1\n"
            self.assertEqual(dm.query(["-Q"]), "gdm 1.0-1\n")
            args, kwargs = run.call_args
            self.assertEqual(args[0], ["/usr/bin/pacman", "-Q"])
            self.assertNotIn("shell", kwargs)
            self.assertEqual(kwargs["stdin"], dm.subprocess.DEVNULL)

    def test_query_rejects_mutations_before_invoking_pacman(self):
        with patch.object(dm.subprocess, "run") as run:
            for arguments in (["-S", "gdm"], ["-R", "gdm"], ["-Qqo", "--", "relative"], None):
                with self.subTest(arguments=arguments), self.assertRaises(dm.ContractError):
                    dm.query(arguments)
            run.assert_not_called()

    def test_cli_rejects_mutable_subject_before_any_inspection(self):
        with patch.object(dm, "Inventory") as inventory, contextlib.redirect_stderr(io.StringIO()):
            for argv in (["--profile", "published-gdm", "--image", "ghcr.io/tuna-os/marlin:tuna",
                          "--platform-digest", SUBJECT["platform_digest"]],
                         ["--profile", "current-greetd", "--image", SUBJECT["image"],
                          "--platform-digest", SUBJECT["platform_digest"]]):
                with self.assertRaises(SystemExit):
                    dm.main(argv)
            inventory.assert_not_called()

    def test_cli_failure_emits_json_and_never_claims_digest_binding(self):
        report = {"static_inventory_ready": False, "runtime_qualified": False, "errors": ["missing GDM"]}
        with patch.object(dm, "Inventory") as inventory, contextlib.redirect_stdout(io.StringIO()) as stream:
            inventory.return_value.collect.return_value = report
            result = dm.main(["--profile", "published-gdm", "--image", SUBJECT["image"],
                              "--platform-digest", SUBJECT["platform_digest"]])
        observed = json.loads(stream.getvalue())
        self.assertEqual(result, 1)
        self.assertFalse(observed["runtime_qualified"])
        self.assertIn("caller-declared", observed["subject"]["binding"])


if __name__ == "__main__":
    unittest.main()
