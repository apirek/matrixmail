# matrixmail
Send messages to matrix rooms instead of email.

```bash
echo "the message" | mail -s "the subject" '!jEsUZKDJdhlrceRyVU:example.org'
```

Messages are send as plain text.

## Installation
Copy or symlink to `/usr/bin/mail` and/or `/usr/bin/mailx`.
Run the binary under a different name to log in.
It will prompt for your login and save the session:
```shell
# (exec -a setup mail)
Homeserver (default: matrix.org): example.org
User: alice
Password: 
Device name (default: localhost): Alice's Computer
Display name (default: Alice@Alice's Computer): 
```

## Usage
Create a room and invite, or start a direct chat with, the matrixmail user. Use that room's ID as recipient address.
The matrixmail user will join when sending its first message.

In the Element Matrix client the room ID can be found in *Room settings* > *Advanced* > *Room information*.

matrixmail will read a message from stdin and send it to all specified rooms.
If a subject is specified it will be the first line of the message, separated from it by a blank line:
```bash
echo "the message" | mail -s "the subject" '!jEsUZKDJdhlrceRyVU:example.org'
```

## Compatibility
Only Send Mode, no Receive Mode or any interactive features.

Lines starting with `~` (tilde) are ignored.

Addresses must be room IDs or aliases, no user IDs.

End-to-end encryption is supported, but matrixmail will trust _everyone_ in the room.

# Reference
The mailx specification: https://pubs.opengroup.org/onlinepubs/9699919799/utilities/mailx.html
