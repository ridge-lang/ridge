# Vendored SQLite amalgamation

`sqlite3.c` and `sqlite3.h` are the official SQLite amalgamation. They are
compiled into `sqlite_nif.c`, the native function that backs the `std.data`
SQLite adapter. The amalgamation is vendored rather than fetched at build time
so a build never depends on the network and the exact source that ships is
fixed and auditable.

- version:   3.45.3
- source id: 2024-04-15 13:34:05 8653b758870e6ef0c98d46b3ace27849054af85da891eb121e9aaa537f1e8355
- origin:    https://www.sqlite.org/2024/sqlite-amalgamation-3450300.zip

SHA-256:

- `sqlite3.c`  `9ca336fbcbff9f1d78b4f45b6a19583fcc097192310dd2f5f6cd43b9a33d7d69`
- `sqlite3.h`  `882ad3c0448d0324fb3a6b1a85333a9173d539ac669c9972ae1f03722ff86282`

Bumping the pin is deliberate: change the version here, re-vendor both files
from sqlite.org, update the hashes, and re-run the adapter test suite before
committing. The version string is also asserted against the running library at
load time, so a mismatched vendor drop fails loudly instead of shipping.
