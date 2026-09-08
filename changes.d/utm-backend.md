---
priority: -40
---
 -  Guests that Tart cannot host are now run through UTM. You never choose
    the virtualizer yourself: each profile names the guest it wants and
    `wargame` picks the one that can run it, so macOS guests keep working
    exactly as before.

 -  `wargame profiles` shows what that resolved to — the guest system and
    architecture, the virtualizer and whether it virtualizes or emulates,
    and the profile's image state — and `wargame doctor` reports the health
    of every virtualizer you actually have profiles for. A session made with
    a profile whose virtualizer this build cannot drive still lists and
    still deletes; only starting it is refused.

 -  The same four answers appear wherever you pick or watch a profile: the
    interactive profile picker shows them next to each name, `wargame launch`
    prints what the profile resolved to and which image it will clone before
    it creates anything, and `wargame sessions` and `wargame session inspect`
    name the virtualizer each session actually runs on. That last one matters
    once macOS and Kali sessions sit in the same list — every stop, resume,
    delete or console goes to the session's own virtualizer, including for
    sessions created before wargame had more than one.
