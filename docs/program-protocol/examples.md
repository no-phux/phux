# Examples

These reports are part of the [program protocol](../program-protocol.md).

Shell, with base64 values decoded in a trailing comment. Write to
`/dev/tty` when standard output may be redirected.

```sh
# One program, root record
printf '\033]7501;state=working:app=brew:msg=SW5zdGFsbGluZyB1cGRhdGVz\033\\'   # Installing updates
printf '\033]7501;state=blocked:kind=auth:app=brew:msg=UGFzc3dvcmQgcmVxdWlyZWQ=\033\\'   # Password required
printf '\033]7501;state=done:app=brew:msg=VXBncmFkZWQgMTIgcGFja2FnZXM=\033\\'   # Upgraded 12 packages

# Several records, then clear one subtree and the rest
printf '\033]7501;state=working:app=deploy\033\\'
printf '\033]7501;state=working:id=us-east:progress=40\033\\'
printf '\033]7501;state=blocked:kind=permission:id=eu-west:msg=QXBwcm92ZSBwcm9kdWN0aW9uPw==\033\\'   # Approve production?
printf '\033]7501;state=clear:id=eu-west\033\\'
printf '\033]7501;state=clear\033\\'
```

A function that encodes the message:

```sh
status() {
  printf '\033]7501;state=%s:app=sync:msg=%s\033\\' "$1" \
    "$(printf '%s' "$2" | base64 | tr -d '\n')" > /dev/tty
}
status working "Syncing photos"
rsync -a ~/Photos backup:/photos && status done "Photos synced" || status error "rsync failed"
```

Ask for support, then fall back after a short wait:

```sh
printf '\033]7501;?\033\\\033[c' > /dev/tty
```

Read the reply from the terminal in raw mode; an answer starting
`ESC ] 7501 ; ?` means supported.
