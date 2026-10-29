---
tags: [testing, accessibility, session-isolation, at-spi]
---

# Isolate accessibility proof sessions

Give each parallel graphical proof its own `XDG_RUNTIME_DIR` with mode `0700`, private session bus, and private accessibility bus. Pass an explicit `AT_SPI_BUS_ADDRESS` to the shell, test clients, and accessibility queries. Record the bus identity in the proof manifest and clean up only resources owned by that proof.

A private session bus alone is insufficient: accessibility activation can advertise a shared socket under the user runtime directory, causing parallel proofs to inspect another session’s accessibility tree or time out.
