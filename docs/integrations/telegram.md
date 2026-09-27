# Telegram notifications

- Status: **implemented** (`qd-server::notify`, ADR 0014).
- Read on 2026-09-27 from https://core.telegram.org/bots/api.
- Every call has the form `https://api.telegram.org/bot<token>/<METHOD>`.
  `sendMessage` takes `chat_id` (integer, or `@channel`) and `text`, and
  returns `{"ok": true, "result": ...}`. Failures return
  `{"ok": false, "description": ...}`.
- Setup:
  1. Create a bot with @BotFather and copy its token into Settings → API keys
     → Telegram bot token.
  2. Send any message to your bot. Find your chat id (for example with
     @userinfobot), then enter it in Settings → Notifications.
  3. Press "Send test message".
- What is sent, only while notifications are switched on:
  - raised alerts at or above `min_severity`;
  - a short summary after each daily paper or live run (`daily_summary`),
    with counts only (no equity);
  - failures of the bar import, the daily runs and the fill checks, once a
    day each.
- The test button sends even while notifications are off.
- The token is part of the URL, so errors are reported without the URL.
  Nothing secret is ever put in a message.
