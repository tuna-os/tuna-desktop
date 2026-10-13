#!/usr/bin/env python3
"""Source routing checks only; Rust sockets/GTK callback execution belongs to CI."""
from pathlib import Path
import unittest
ROOT=Path(__file__).resolve().parents[2]
class Contract(unittest.TestCase):
 def test_current_unlock_uses_dedicated_encoder_and_never_reconnects_or_replays(self):
  text=(ROOT/'crates/shell-host/src/control.rs').read_text()
  body=text.split('pub fn unlock(',1)[1].split('\n    ///',1)[0]
  self.assertIn('encode_unlock_frame',body);self.assertIn('Shutdown::Both',body)
  self.assertNotIn('write_message',body);self.assertNotIn('reconnect()',body)
  self.assertIn('auth_needs_snapshot',body)
 def test_initial_snapshot_and_pr_checkout_admission(self):
  text=(ROOT/'crates/shell-host/src/control.rs').read_text()
  self.assertIn('auth_needs_snapshot: true',text)
  proof=(ROOT/'scripts/tuna-auth-no-replay-gtk-proof').read_text()
  self.assertNotIn('GITHUB_SHA',proof)
  self.assertIn('identity.current_identity(root)',proof)
  self.assertIn('owned_cleanup',proof)
  self.assertIn('lifetime.bounded_command',proof)
  workflow=(ROOT/'.github/workflows/ci.yml').read_text()
  for job,next_job in [('check','content-tests'),('gtk-shell','portal-security')]:
   block=workflow.split('  '+job+':',1)[1].split('\n  '+next_job+':',1)[0]
   self.assertIn('ref: ${{ github.event.pull_request.head.sha || github.sha }}',block)
 def test_generic_writer_refuses_credentials_before_generic_encoder(self):
  text=(ROOT/'crates/shell-host/src/control.rs').read_text().split('fn write_message(',1)[1]
  self.assertLess(text.index('CommandKind::Unlock'),text.index('encode_frame'))
 def test_actual_consumer_clears_fields_and_waits_for_authoritative_lock(self):
  lock=(ROOT/'crates/shell-gtk/src/lock.rs').read_text()
  self.assertIn('None => reset_prompt(entry, message)',lock)
  self.assertIn('entry.set_text("")',lock)
  main=(ROOT/'crates/shell-gtk/src/main.rs').read_text()
  self.assertIn('sync_connection_generation(auth_generation)',main)
  self.assertIn('filter(|c| !c.needs_snapshot())',main)
  self.assertIn('if let Some(locked) = authoritative_lock',main)
if __name__=='__main__':unittest.main()
