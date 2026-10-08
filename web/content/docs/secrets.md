---
title: Passwords & secrets
description: One encrypted authinfo.gpg store for every package — master passphrase, masked prompts, secrets as option values, and passing them to subprocesses without exposing them in argv.
---

# Passwords & secrets

Macros keeps passwords, API keys, and other secrets in **one encrypted file**, `~/.config/macros/authinfo.gpg`, shared by every package. It works like Emacs's `auth-source` with `~/.authinfo.gpg`. You type a master passphrase once per session, and packages ask the store instead of prompting you themselves. Secrets never need to appear in `init.scm`.

The store needs **gpg**, which does the encryption (`brew install gnupg` on macOS). The file is a normal symmetric gpg file, so `gpg -d ~/.config/macros/authinfo.gpg` and Emacs both open it.

## The file

It's authinfo (netrc) text, one entry per line, encrypted with your master passphrase:

```
machine db.example.com login kyle port 5432 password hunter2
machine imap.gmail.com login me@example.com password "has spaces"
machine psql-url-1 login kyle password postgres://kyle:s3cret@db.example.com/app
```

An entry is looked up by **machine** (a real hostname, or any name you choose) plus **login**, optionally narrowed by **port**. Two keys are needed because one host can have several accounts. An empty login or port in a lookup matches any entry for that machine. Lines that aren't entries (comments, blanks, `default …`) are kept as-is when the editor rewrites the file.

You don't create the file by hand. The first time you save a secret, Macros asks for a **new** master passphrase (twice) and writes the file, readable only by you (mode `0600`). Each later save re-encrypts it atomically: a temp file is renamed over the old one.

## What you see

The first time a command needs a secret in a session:

1. **`authinfo.gpg passphrase:`** You type the master passphrase once. After that, lookups are answered from memory until you quit or run `M-x secrets-lock`. A wrong passphrase asks again, up to three tries.
2. If there's **no entry**, you get a prompt like **`Password for kyle@db.example.com:5432:`**.
3. **`Save kyle@db.example.com:5432 to authinfo.gpg? (y/n)`** If you answer `n`, the value is still remembered until you quit or run `secrets-lock`, so you're not asked again on every command.

Secret prompts are masked (`•` per character). What you type is never added to minibuffer history, and on macOS **Secure Event Input** is on while the prompt is open, so other apps can't watch the keystrokes. <kbd>cmd-v</kbd> and <kbd>C-y</kbd> paste. In a secret prompt both read the clipboard directly and never put the secret on the kill ring (which [session state](/docs/configuration#session-state) may save to disk). A trailing newline is dropped, and only the first line of a multi-line paste goes in. <kbd>C-g</kbd> cancels.

If a service later **rejects** a stored password because it was changed or expired, the package deletes that entry, and the next command asks for it again.

## Commands

| Command | What it does |
| --- | --- |
| `secrets-unlock` | Enter the master passphrase now, ahead of the first command that needs it |
| `secrets-lock` | Forget the decrypted store and any unsaved secrets from this session |
| `secrets-add` | Store an entry ahead of time: machine, login, port (both optional), then the secret |
| `secrets-change-passphrase` | Re-encrypt the file with a new master passphrase |

To edit the file by hand, decrypt it with gpg, edit it, and encrypt it again with the same passphrase (`gpg -c`).

## Options

```scheme
(set-option "authinfo-file" "~/.config/macros/authinfo.gpg")  ; the default
(set-option "gpg-command"   "/opt/homebrew/bin/gpg")          ; default: auto-detect
```

The editor is a GUI app, so it doesn't inherit your shell's `PATH`. `gpg-command` defaults to the first of `/opt/homebrew/bin/gpg`, `/usr/local/bin/gpg` and `/usr/bin/gpg` that exists, then to `gpg` on `PATH`.

## Secrets in your config

Any option can be set to a **secret reference** instead of a value. Call `secret-get` with a machine and a login and **no callback**:

```scheme
(set-option "psql-url" (secret-get "psql-url-1" "kyle"))
```

- **Only the reference is stored**, never the secret. It prints as `#<secret kyle@psql-url-1>`. Your `init.scm` and the option table never contain the value, and loading your config doesn't prompt.
- **`(get-option "psql-url")` returns the real value** once the store (or this session) has it, and `#f` until then.
- **The first command that needs it prompts.** A package gates its commands with `option-ensure` (below), which asks for the master passphrase and, if there's no entry yet, for the value itself, masked (`psql-url (kyle@psql-url-1):`). It then offers to save it.

Pass `'port "5432"` as a third argument to narrow the lookup to a port.

For example, the `macros-psql-dashboard` package keeps its whole connection URL in the store this way:

```scheme
(package-install-local "~/workspace/macros-psql-dashboard"
  'config (lambda ()
            (set-option "psql-command" "/opt/homebrew/opt/libpq/bin/psql")
            (set-option "psql-url" (secret-get "psql-url-1" "kyle"))
            #f))
```

## Using the store from a package

Never prompt for a password yourself. Ask the store, and let it handle prompting, saving and remembering.

### `secret-get`: get a secret, prompting if needed

```scheme
(secret-get "db.example.com" "kyle"
  (lambda (pw) (connect pw))                       ; called with the secret
  'port "5432"                                     ; optional extra match key
  'on-cancel (lambda (_) (message "cancelled"))    ; optional: the user gave up
  'prompt "Token for db.example.com: ")            ; optional: replaces "Password for …: "
```

This is asynchronous. The callback runs once the secret is available: straight away on a hit, or after the prompts on a miss. Requests are handled one at a time, so two packages asking at once get their prompts in turn rather than one replacing the other. If the callback throws, the error is reported **without** its message, because Steel's error text quotes the offending value, which could be the secret.

### `secret-lookup`: synchronous, never prompts

```scheme
(secret-lookup "db.example.com" "kyle" "5432")   ; → the secret, or #f
```

This answers only from the unlocked store and this session's memory. Use it in code that must stay synchronous, such as a completion source or a catalog query, after gating the command that led there with `secret-get`.

### `secret-forget`: the service rejected it

```scheme
(secret-forget "db.example.com" "kyle" "5432")
```

This deletes the entry from session memory, the unlocked store, and `authinfo.gpg`, so the next `secret-get` prompts again.

### Options that may be secret references

```scheme
(option-ensure "my-pkg-url"                 ; run k once the option can be read
  (lambda () (my-pkg-connect (get-option "my-pkg-url")))
  'on-cancel (lambda (_) (message "cancelled")))

(option-secret-label "my-pkg-url")          ; → "kyle@my-pkg-1" if it's a reference, else #f
(option-secret-forget "my-pkg-url")         ; rejected: forget the entry it refers to → #t / #f
(secret-ref? v)                             ; is v a secret reference?
```

`option-ensure` runs `k` right away for an ordinary or unset option. A synchronous `get-option` of a reference that isn't available yet returns `#f`; it never prompts. While it's `#f`, **don't fall back to a default.** Refuse to run instead, so a locked secret can't silently send your command to the wrong place.

### The pattern

Gate each user-facing command with `secret-get` (or `option-ensure`), then do the synchronous work with `secret-lookup`:

```scheme
(define-command (my-db-report)
  (secret-get "db.example.com" "kyle"
    (lambda (_) (my-db--render))      ; inside, (secret-lookup …) now answers
    'port "5432"))
```

## Handing a secret to a subprocess

**Never put a secret in argv.** Every local user can read a process's command line with `ps`. Pass it through the child's environment instead, which other users can't read:

```scheme
;; Synchronous — like call-process-full, plus env for the child only:
(call-process-env "psql" (list "-d" url "-c" "select 1") ""
                  (list (string-append "PGPASSWORD=" pw)))

;; Asynchronous — like make-process:
(make-process-env "psql" args 'on-output 'on-exit
                  (list (string-append "PGPASSWORD=" pw)))

;; Or for everything spawned inside a block (call-process*, make-process*,
;; make-terminal, dap-start):
(with-process-env (list (string-append "PGPASSWORD=" pw))
  (lambda () (call-process-full "psql" args "")))
```

`env` is a list of `"KEY=VALUE"` strings layered over the editor's own environment, which never changes. A bare `"KEY"` removes that variable for the child; in a terminal, where removal isn't possible, it's set to empty instead. See [Scripting](/docs/scripting#running-processes).

If a secret comes as part of a URL (`postgres://kyle:s3cret@host/db`), split the password out before spawning and pass it through the environment. `(url-decode s)` decodes `%XX` escapes, as the client itself would.

## Reading a secret yourself

Only use this when you really must prompt directly, for a one-off secret that shouldn't be stored:

```scheme
(read-passwd "Token: "
  (lambda (token) (use token))
  (lambda (_) (message "cancelled")))   ; optional: C-g, or another prompt replaced it
```

It's the same masked, history-free, Secure Input prompt, with paste behaving as above. The input reaches the callback exactly as typed, not trimmed, since spaces can be part of a password. Prefer `secret-get` so the store can remember and save the secret.
