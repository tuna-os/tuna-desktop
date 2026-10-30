# Fingerprint prompt guards

Each adapter can accept a password. They reject prompts that the classifier
marks as fingerprint prompts.
They do not start a fingerprint scan.

The adapter for unlock through greetd cancels the conversation before it sends an answer
for a marked Visible or Secret prompt. The backend for private PAM checks the
whole challenge batch before it constructs any answers. A marked Visible or
Secret message denies the batch, discards the owned credential, and follows
the existing cancellation and physical retirement path. Info and Error
messages still receive no answer.

The greeter controller also rejects typed submissions for marked prompts.
The GTK consumer hides and clears the answer field and focuses Cancel. Its
notice states that typed answers are unavailable. It makes no claim that a
reader is active. A fresh ordinary conversation can accept a fresh password
once cancellation has completed.

## Classifier limits

All three consumers use the existing English text classifier in the greeter
model. It recognizes fingerprint, fprint, swipe, and selected finger/reader
phrases. It cannot identify every localized or vendor-specific prompt.
Unknown prompts keep the existing behavior. This guard does not provide a
method-aware challenge broker or full MFA support.

## Verification scope

The source adds Rust unit regressions for typed rejection, cancellation,
normal password recovery, Info/Error responses, and whole-batch admission.
The existing GTK/greetd CI walkthrough adds a marked Secret prompt. It
invokes the real button and entry callbacks and needs CancelSession as
the next wire request. An Answer request fails the peer assertion. The
existing watchdog and normal password/token controls remain in place.

These regressions must execute in CI. A source review and a format check alone do
not qualify the live path. The controlled greetd peer does not qualify a
reader, installed PAM stack, suspend/resume, or issue #366.

The d371 successor changes the APIs for cancellation and wake. This
patch changes no cancellation API. The guard for message admission must run
before answer construction in that successor too. These guards apply to the
single-password adapters only; the interactive conversation relays every
prompt to the user and fingerprint runs as its own service
([lock-conversations.md](lock-conversations.md)).
