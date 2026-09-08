---
priority: -40
---
 -  Guests that Tart cannot host are now run through UTM. You never choose
    the virtualizer yourself: each profile names the guest it wants and
    `wargame` picks the one that can run it, so macOS guests keep working
    exactly as before.

 -  `wargame profiles` shows what that resolved to -- the guest system and
    architecture, the virtualizer and whether it virtualizes or emulates,
    and the profile's image state -- and `wargame doctor` reports the health
    of every virtualizer you actually have profiles for. A session made with
    a profile whose virtualizer this build cannot drive still lists and
    still deletes; only starting it is refused.
