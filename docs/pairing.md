# Pairing

Each profile on the handheld is paired with one agent: a Muse, or a Hermes Agent you run
yourself. Add a profile in **Settings → Profiles**, or press A on the first start.

## With Muse

1. Connect the handheld to Wi-Fi with internet access, and put your phone on the same
   network.
2. Choose **Muse**. Scan the QR code on the handheld with your phone.
3. The page walks you through **Get an SDK token → Save it on the handheld → Pair in Muse**.
   Reuse a token you saved before for this Muse account, or open **Get or manage SDK
   tokens**, copy one, and come back. Tap **Save token**.
4. With Bluetooth on, open **Muse → Settings → Devices** on your phone. Turn on
   **Developer mode**, tap **+**, and choose the device the handheld shows. Keep the
   handheld's pairing screen open. If Muse asks for Wi-Fi, choose the network the handheld
   is on.

Saving the token alone does not pair the profile: step 4 is needed.

### The SDK token

Meta issues SDK tokens to Muse account holders at
[gadgets.muse.ai/settings/sdk-tokens](https://gadgets.muse.ai/settings/sdk-tokens). This
project does not come with one and cannot give you one. Tokens come with Meta's
[Gadget SDK Token Terms of Use](https://gadgets.muse.ai/sdk-terms): among other things, they
are for personal, non-commercial use with your own Muse account.

- **One token per profile.** Profiles can hold the same token or different ones. Each paired
  profile is a separate device to Muse, and Muse limits how many devices one token can pair.
- **Keep a copy somewhere safe.** Replacing or revoking a token can disconnect the devices
  paired with it. Saving a different token does not change an existing pairing: pair that
  profile again.
- **Without the phone page:** if the phone and the handheld cannot be on the same network,
  save the token in a file named `sdk_token.txt` in the `GoblinLink` folder on the SD card,
  then pair.

The page you paste into is served by the handheld itself, over plain HTTP on your own Wi-Fi,
only while that screen is open. Someone else on the same network could read what you
send, so pair on a network you trust ([SECURITY.md](../SECURITY.md#pairing)).

## With a Hermes Agent

1. Install the Goblin Link plugin on the machine Hermes runs on:
   [hermes-plugin/README.md](../hermes-plugin/README.md). The handheld must be able to reach
   that machine, on the same network or over a VPN such as Tailscale.
2. On the handheld, choose **Hermes Agent** and scan the QR code with your phone.
3. Send your Hermes `/goblinlink pair` in a private chat. Paste the link it answers with on
   the page your phone opened, and name the profile there if you like.

There is no Bluetooth step. The link works once, for ten minutes; treat it like a password
until it is used. `/goblinlink devices` lists the handhelds paired with your Hermes, and
`/goblinlink unpair <id>` removes one. Read
[SECURITY.md](../SECURITY.md#a-profile-paired-with-a-hermes-agent) before you pair.

## Switching profiles

Press **Menu**, go to **Profiles** with R1, select a profile with A, and choose **Switch to
this profile**.

- Only one profile is connected at a time.
- Switching restarts the connection. Wait for it to finish before switching again.
- An unpaired profile stays unpaired when selected.
- Cancel an open pairing before switching.

## If pairing fails

- **The page will not open:** check that the phone and the handheld are on the same
  network, and that the network lets its devices reach each other (guest networks often do
  not). The QR screen also shows an address to type in by hand.
- **Pairing failed:** fix the connection, then press **A** to try again or **B** to go back.
- **"Can't reach Muse":** the handheld is on the network but could not reach Muse. It does
  not mean Wi-Fi is off.
- **The first Connect in the Muse app fails:** this happens often; the second try usually
  works.
