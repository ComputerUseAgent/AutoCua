# Vault

Saved credentials for the agent's `vault` action. The agent fills a login field
straight from here, so the secret is typed into the app without ever entering
the model's context.

This folder lives outside the install directory on purpose — the uninstaller
wipes all of `{app}`, and your credentials should survive that.

## credentials.json

One file for every device, with a section each. It is created the first time
the app runs (`vault_file()` in `AutoCua/__init__.py`) with empty sections.
Fill it in yourself:

```json
{
  "ios": {
    "Instagram": {
      "username": "you@example.com",
      "password": "..."
    },
    "Revolut": {
      "phone": "+441234567890",
      "pin": "1234"
    },
    "iPhone": {
      "pin": "123456"
    }
  },
  "web": {
    "github.com": {
      "email": "you@example.com",
      "password": "..."
    },
    "mybank.com": {
      "username": "you",
      "password": "...",
      "pin": "1234"
    }
  }
}
```

Each agent reads only its own section, so an iOS app name and a web site never
mix. A `"computer"` section (desktop apps) will join them later.

## How both work

- The nested keys are yours to name. The agent sees only their names, as
  `<vault app="Instagram">username, password</vault>` or
  `<vault url="github.com">email, password</vault>`, and calls
  `vault {"id": 5, "value": "password"}` to fill one. It answers
  `filled [5] (Password) with the saved password for Instagram`.
- Values are text (a PIN with a leading zero must be in quotes). A key with
  `password` in its name goes only into a password field.
- A value it typed shows as `[vault]` to the agent afterwards, except in the
  screenshot, where an email or PIN is still visible (password fields show
  dots).

## "ios": the iPhone agent

- Keys are **app names**, matched exactly with case and spaces ignored: write
  the name the app shows (`"Disney+"`, not `"Disney Plus"`). When the
  `<vault>` block does not show for an app, its name does not match.
- Right before typing, the app that was scanned must still be in front and the
  field that has focus must be a text field (a password field for a password),
  or nothing is typed.
- `"pin"` on a PIN keypad (the lock screen, an app's digit pad) is tapped key
  by key, never typed: `vault {"id": 0, "value": "pin"}`, alone in its step. A
  PIN text field is filled like any other field.
- `"iPhone"` / `"iPad"` hold the device passcode, used only on the lock screen.

## "web": the browser agent

- Keys are **sites**. A site covers itself and its subdomains: `google.com`
  also covers `accounts.google.com`. The longest matching site wins.
- It fills only on https pages (http only on localhost), and checks the page
  is still on that site right before typing. Nothing is typed into a field
  inside a frame.

## Security

`credentials.json` is **git-ignored and must stay that way** — see the
`/AutoCua_data/vault/*` rules in `.gitignore`. Only this README is tracked.
Never commit the JSON, and never paste its contents into a chat, an issue, or a
log. The file is plaintext on disk: it is only as protected as your user
account, so treat it like `~/.ssh/`. One typo in it stops the vault on every
device, so keep it valid JSON.
