# Vault

Saved credentials for the agent's `vault` action. The agent fills a login field
straight from here, so the secret is typed into the app without ever entering
the model's context.

This folder lives outside the install directory on purpose — the uninstaller
wipes all of `{app}`, and your credentials should survive that.

## credentials.json

Created automatically as `{}` the first time the app runs
(`vault_file()` in `AutoCua/__init__.py`). Fill it in yourself:

```json
{
  "Instagram": {
    "username": "you@example.com",
    "password": "..."
  },
  "Gmail": {
    "email": "you@example.com",
    "password": "...",
    "phone": "+441234567890"
  },
  "Revolut": {
    "phone": "+441234567890",
    "pin": "1234"
  }
}
```

Top-level keys are **app names**. The nested keys the lookup understands are:

| key        | filled when the target field's label contains |
| ---------- | --------------------------------------------- |
| `username` | `username` or `email`                         |
| `email`    | `username` or `email` (tried before `username`) |
| `password` | `password` — or the field is a `SecureTextField` |
| `phone`    | `phone`                                       |
| `code`     | `code` or `pin`                               |
| `pin`      | `code` or `pin` (tried after `code`)          |

Any other key is ignored. A field with no matching key makes the action fail
with *"No matching credential found"* rather than typing the wrong thing.

## How a lookup works

`VaultService.get_credential_for_element()` reads the current element tree:

1. Element `[1]` is the application — its `element_name=` (iOS) or `label=`
   (older trees) gives the app name.
2. That name is fuzzy-matched against the top-level keys here, so `"Instagram"`
   in this file still matches an app reporting itself as `"Instagram "` or
   `"instagram"`. Matching is case- and space-insensitive and accepts either
   name containing the other, or a shared 4-character prefix.
3. The target element's own label picks the field, per the table above.

## Security

`credentials.json` is **git-ignored and must stay that way** — see the
`/AutoCua_data/vault/*` rules in `.gitignore`. Only this README is tracked.
Never commit the JSON, and never paste its contents into a chat, an issue, or a
log. The file is plaintext on disk: it is only as protected as your user
account, so treat it like `~/.ssh/`.
