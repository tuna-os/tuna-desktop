---
created_date: "2026-09-28"
document_status: final
closed_date: "2026-09-28"
---

# Test plan: 20260928093027-31646037-greetd-greeter

Manual procedures for the success metrics CI cannot cover (no live
PAM, no cold boot in CI). Automated coverage lives in the test suite
(`crates/greeter`: model, exchanges, walkthrough). Full background in
`docs/greeter-vm-acceptance.md`.

## Cold boot to login screen on every boot

- **Measure**: login window appears with user list and session picker
  on 5 consecutive cold boots, zero manual recovery steps.
- **How**: VM with greetd configured to launch `rwd-greeter`
  (`target/debug/rwd-greeter` until packaged); power-cycle, observe.
- **Expected**: 5/5 boots show the picker with RWD default.
- **Who/when**: maintainer, pre-release and after greeter changes.

## Sign-in success with real credentials, zero shell drops

- **Measure**: 20 consecutive valid sign-ins succeed; 10 bad-password
  attempts all return the generic notice with no shell, console, or
  distinguishing error.
- **How**: on the VM, sign in as a real user; then with wrong
  passwords. Repeat after `systemctl restart greetd` once mid-series.
- **Expected**: 20/20 success, 10/10 safe failures.
- **Who/when**: maintainer, pre-release.

## Every listed session launches

- **Measure**: each session file in the picker launches at least once
  to an input-receiving window.
- **How**: on the VM, sign in once per listed entry (RWD plus any
  others installed).
- **Expected**: all entries launch; failures return to login with the
  plain-language notice (dead-session path).
- **Who/when**: maintainer, pre-release.
