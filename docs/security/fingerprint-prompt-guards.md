# Fingerprint prompt guards

Each adapter can accept a password. They reject prompts that the classifier
marks as fingerprint prompts.
They do not start a fingerprint scan.

The unlock conversation adapter (`unlock::attempt_unlock`) cancels the
conversation before it sends an answer for a marked Visible or Secret prompt. The backend for private PAM checks the
whole challenge batch before it constructs any answers. A marked Visible or
Secret message denies the batch, discards the owned credential, and follows
the existing cancellation and physical retirement path. Info and Error
messages still receive no answer. The PAM conversation of the single-password
path fails a marked prompt instead of answering it with the password.

## Classifier limits

All three consumers use the English text classifier
`pam::is_fingerprint_prompt`. It recognizes fingerprint, fprint, swipe, and selected finger/reader
phrases. It cannot identify every localized or vendor-specific prompt.
Unknown prompts keep the existing behavior. This guard does not provide a
method-aware challenge broker or full MFA support.

## Verification scope

The source adds Rust unit regressions for typed rejection, cancellation,
normal password recovery, Info/Error responses, and whole-batch admission.

These regressions must execute in CI. A source review and a format check alone do
not qualify the live path. They do not qualify a reader, installed PAM stack,
suspend/resume, or issue #366.

The d371 successor changes the APIs for cancellation and wake. This
patch changes no cancellation API. The guard for message admission must run
before answer construction in that successor too. These guards apply to the
single-password adapters only; the interactive conversation relays every
prompt to the user and fingerprint runs as its own service
([lock-conversations.md](lock-conversations.md)).
