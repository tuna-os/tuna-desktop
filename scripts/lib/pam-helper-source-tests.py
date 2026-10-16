#!/usr/bin/env python3
"""Source integration guards; actual PAM execution is a separate CI Rust test."""
from pathlib import Path
import unittest
import re
ROOT = Path(__file__).resolve().parents[2]

class SourceContracts(unittest.TestCase):
    def test_fixed_helper_entry_precedes_any_desktop_initialization(self):
        main = (ROOT / 'crates/compositor/src/main.rs').read_text().split('fn main() -> ExitCode {', 1)[1]
        self.assertLess(main.index('pam_helper::entry()'), main.index('legacy::import_env'))
        self.assertLess(main.index('pam_helper::entry()'), main.index('NestedSession::default_for_pid'))
        self.assertIn('std::env::args_os().count() != 2', main)

    def test_helper_is_authentication_only_and_live_factory_is_held_self(self):
        helper = (ROOT / 'crates/compositor/src/pam_helper.rs').read_text()
        for symbol in ('pam_authenticate', 'pam_acct_mgmt', 'pam_end', 'pam_get_item'):
            self.assertIn(symbol, helper)
        for symbol in ('pam_open_session', 'pam_setcred', 'StartSession'):
            self.assertNotIn(symbol, helper)
        backend = (ROOT / 'crates/compositor/src/pam_backend.rs').read_text()
        self.assertIn('SelfImage::capture()', backend)
        live = backend.split('pub fn live() -> Option<Self> {', 1)[1].split('/// Trusted Rust fixture seam,', 1)[0]
        self.assertIn('let image = SelfImage::capture().ok()?;', live)
        self.assertIn('move |identity, service, user, budget, cancel|', live)
        self.assertIn('image.spawn_cancellable(identity, service, user, budget, cancel)', live)
        self.assertNotIn('Supervisor::spawn', live)
        self.assertNotIn('Cancellation::default()', live)
        cancellable = helper.split('pub fn spawn_cancellable(', 1)[1].split('/// Blocking foundation API:', 1)[0]
        self.assertIn('Supervisor::spawn_inner(', cancellable)
        self.assertIn('Some(self.file.clone())', cancellable)
        self.assertIn('Some(cancel)', cancellable)
        self.assertIn('File::open("/proc/self/exe")', helper)
        self.assertIn('self.file.as_raw_fd()', helper)
        self.assertIn('_source_image: source_image', helper)
        self.assertNotIn('std::env::', backend)
        pam = (ROOT / 'crates/compositor/src/pam.rs').read_text()
        self.assertNotIn('std::thread::Builder', pam)
        self.assertIn('crate::pam_backend::Backend::live()', pam)
        runtime = (ROOT / 'crates/compositor/src/runtime.rs').read_text()
        self.assertIn('if !result.may_retire()', runtime)
        self.assertIn('self.unlock_quarantined = Some(attempt)', runtime)
        self.assertIn('let (reported_ok, ok) = unlock_outcome(backend_accepted, denied);', runtime)
        self.assertIn('(authenticated, authenticated && !denied)', runtime)
        self.assertIn('worker.phase.load', runtime)
        self.assertIn('ticket.wait_retired', runtime)
        self.assertIn('verify_owned', (ROOT / 'crates/compositor/src/unlock.rs').read_text())

    def test_interactive_relay_is_internal_fresh_bounded_and_in_actual_helper_fixture(self):
        relay = (ROOT / 'crates/compositor/src/pam_conversation.rs').read_text()
        backend = (ROOT / 'crates/compositor/src/pam_backend.rs').read_text()
        helper = (ROOT / 'crates/compositor/src/pam_helper.rs').read_text()
        for token in ('pub const INACTIVITY', 'pub struct Policy', 'pub struct Binding',
                      'pub struct Challenge', 'pub struct ResponseHandle', 'valid_answers',
                      'self.cancel.fd()', 'libc::poll', 'self.total_deadline', 'Input::Activity'):
            self.assertIn(token, relay)
        self.assertNotIn('tuna_shell_control', relay)
        self.assertNotIn('is_fingerprint_prompt', relay)
        self.assertIn('pub fn begin_conversation(', backend)
        self.assertIn('image.spawn_conversation(identity, service, user, budget, cancel)', backend)
        self.assertIn('deadline.min(self.total_deadline)', helper)
        actual = (ROOT / 'crates/compositor/tests/pam_helper.rs').read_text()
        self.assertIn('return run_interactive_relay_cases();', actual)
        for case in ('fresh-multistep', 'wrong-second', 'wrong-origin', 'wrong-request', 'wrong-epoch', 'wrong-sequence', 'cancel-pending', 'shutdown-pending', 'phase-hang'):
            self.assertIn('"' + case + '"', actual)
        self.assertIn('Duration::from_secs(45)', actual)

    def test_consumer_foundation_routes_child_identity_before_poll_and_retirement(self):
        runtime = (ROOT / 'crates/compositor/src/runtime.rs').read_text()
        self.assertEqual(runtime.count('self.control.set_auth_owner('), 2)
        first = runtime.index('self.control.set_auth_owner(')
        self.assertLess(first, runtime.index('let outcome = self.control.poll', first))
        self.assertIn('self.shell\n                .child_pid()\n                .zip(self.shell.child_generation())', runtime)
        self.assertIn('self.unlock_slot.retire_pam_method(', runtime)
        owner = (ROOT / 'crates/compositor/src/auth_owner.rs').read_text()
        self.assertIn('if self.active.is_some()', owner.split('pub fn grant(', 1)[1])
        self.assertIn('method == MethodKind::Fingerprint', owner)
        control = (ROOT / 'crates/compositor/src/control.rs').read_text()
        for name in ('AuthLease', 'bind_auth_lease', 'auth_lease_accepts', 'retire_pam_auth_lease',
                     'auth_lease_binds_socket_child_generation_and_retirement_not_reply'):
            self.assertIn(name, control)
        supervise = (ROOT / 'crates/compositor/src/supervise.rs').read_text()
        reset = supervise.split('pub fn reset_budget(', 1)[1].split('}', 1)[0]
        self.assertNotIn('child_generation', reset)
        self.assertIn('generation.checked_add(1)', supervise)

    def test_mailbox_publishes_then_wakes_and_closes_sender_before_disconnect_notice(self):
        relay = (ROOT / 'crates/compositor/src/pam_conversation.rs').read_text()
        challenge = relay.split('self.challenges\n', 1)[1].split('let answers =', 1)[0]
        self.assertLess(challenge.index('try_send'), challenge.index('self.notice.signal()'))
        backend = (ROOT / 'crates/compositor/src/pam_backend.rs').read_text()
        drop = backend.split('impl Drop for ReplyPublisher', 1)[1].split('struct Job', 1)[0]
        self.assertLess(drop.index('self.sender.take()'), drop.index('notice.signal()'))
        self.assertIn('self.outcome.is_some() || self.ticket.denied', backend)
        actual = (ROOT / 'crates/compositor/tests/pam_helper.rs').read_text().split('fn run_interactive_relay_cases()', 1)[1]
        self.assertIn('ticket.take_notice_source()', actual)
        self.assertIn('event_loop', actual)
        self.assertNotIn('std::thread::sleep', actual)
        self.assertIn('if attempt == 0 { 30 } else { 2 }', actual)
        self.assertIn('if attempt == 0 { 900 } else { 300 }', actual)

    def test_coalesced_notice_drains_bounded_queue_and_fixture_checks_after_reads(self):
        backend = (ROOT / 'crates/compositor/src/pam_backend.rs').read_text()
        poll = backend.split('impl ConversationTicket {', 1)[1].split('pub enum Initial', 1)[0]
        self.assertIn('for _ in 0..2', poll)
        pending = poll.split('Ok(Reply::Pending) => {', 1)[1].split('}', 1)[0]
        self.assertIn('self.cancel();', pending)
        self.assertIn('continue;', pending)
        self.assertEqual(backend.count('job.reply.send(Reply::Pending)'), 1)
        self.assertIn('coalesced_pending_retired_disconnect_is_drained_by_one_owned_callback', backend)
        actual = (ROOT / 'crates/compositor/tests/pam_helper.rs').read_text().split('fn run_interactive_relay_cases()', 1)[1]
        next_case = actual.split('fn next(', 1)[1].split('fn finished(', 1)[0]
        self.assertLess(next_case.index('let challenge = ticket.try_challenge();'),
                        next_case.index('challenge observation exceeded deadline'))
        self.assertLess(next_case.index('challenge observation exceeded deadline'),
                        next_case.index('return challenge;'))
        finished = actual.split('fn finished(', 1)[1].split('let (created, creators)', 1)[0]
        self.assertLess(finished.index('let outcome = ticket.try_outcome();'),
                        finished.index('retirement observation exceeded deadline'))
        self.assertLess(finished.index('retirement observation exceeded deadline'),
                        finished.index('return;'))

    def test_owned_credential_wrap_precedes_early_denials_and_moves_answer(self):
        backend = (ROOT / 'crates/compositor/src/pam_backend.rs').read_text()
        self.assertIn('impl Drop for Credential', backend)
        self.assertIn('crate::pam_helper::wipe(self.text.as_mut_vec())', backend)
        self.assertIn('AnswerText::new(std::mem::take(&mut self.text))', backend)
        self.assertIn('Credential::from_schema(password)', backend)
        pam = (ROOT / 'crates/compositor/src/pam.rs').read_text()
        self.assertIn('text.map(crate::pam_backend::Credential::from_string)', pam)
        self.assertIn('begin_credential', pam)
        runtime = (ROOT / 'crates/compositor/src/runtime.rs').read_text()
        begin = runtime.index('if let Some((origin, password)) = outcome.unlock')
        self.assertLess(runtime.index('Credential::from_schema(password)', begin), runtime.index('self.unlock_slot.begin', begin))

    def test_private_framing_retirement_and_redaction_contracts(self):
        helper = (ROOT / 'crates/compositor/src/pam_helper.rs').read_text()
        for token in ('deny_unknown_fields', 'FRAME_LIMIT', 'TEXT_LIMIT', 'MESSAGE_LIMIT',
                      'checked_duration_since', 'pidfd_open', 'Retirement::Quarantined',
                      'group_has_no_live_members', 'same_user', 'RLIMIT_CORE', '<redacted>'):
            self.assertIn(token, helper)
        self.assertNotIn('.env(', helper)

    def test_raw_negative_transport_is_bounded_before_body_allocation(self):
        test = (ROOT / 'crates/compositor/tests/pam_helper.rs').read_text()
        self.assertIn('fn raw_read<T: Read + AsRawFd>', test)
        self.assertIn('checked_duration_since(Instant::now())', test)
        self.assertIn('(1..=65536).contains(&count)', test)
        self.assertNotIn('output.read_exact(', test)
        self.assertNotIn('stderr.take().unwrap().read_to_string', test)
        self.assertIn('struct RawChild', test)
        self.assertIn('helper outlived its creating thread', test)

    def test_actual_ci_fixture_binds_binary_and_is_not_a_model_only_test(self):
        test = (ROOT / 'crates/compositor/tests/pam_helper.rs').read_text()
        fixture = (ROOT / 'crates/compositor/tests/fixtures/pam_conversation.c').read_text()
        self.assertIn('CARGO_BIN_EXE_tuna-compositor', test)
        self.assertIn('TUNA_REQUIRE_PAM_WRAPPER', test)
        self.assertIn('LD_PRELOAD', test)
        for case in ('hang', 'teardown', 'crash', 'cancel', 'oversized-header', 'stale-generation', 'dribble', 'userchange', 'child'):
            self.assertIn('"' + case + '"', test)
        for token in ('PAM_PROMPT_ECHO_ON', 'PAM_PROMPT_ECHO_OFF', 'PAM_TEXT_INFO', 'PAM_ERROR_MSG', 'pam_set_data', 'fork()'):
            self.assertIn(token, fixture)
        ci = (ROOT / '.github/workflows/ci.yml').read_text()
        gtk = re.search(r'(?ms)^  gtk-shell:\n(.*?)(?=^  [A-Za-z0-9_-]+:\n|\Z)', ci)
        self.assertIsNotNone(gtk)
        self.assertIn('libpam-wrapper libpam0g-dev', gtk.group(1))
        for name in ('ci.yml', 'measured-coverage.yml'):
            self.assertIn('libpam0g-dev', (ROOT / '.github/workflows' / name).read_text())

if __name__ == '__main__': unittest.main()
