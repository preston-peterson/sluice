---
name: Bug report
about: Something isn't working as expected
title: ""
labels: bug
---

**What happened**
A clear description of the bug.

**What you expected**
What you thought would happen instead.

**Steps to reproduce**
1.
2.
3.

**Which part**
- [ ] Desktop app (`sluice-ui`)
- [ ] Engine service (`sluice-engine`)
- [ ] Not sure

**Environment**
- Sluice version (header top-right, or Settings → Updates):
- Install method: `.deb` release / `./install.sh` from source
- Ubuntu/distro version:
- Desktop + session: GNOME, **Wayland** or **X11**

**Logs / output**
- Engine: `journalctl -u sluice-engine -n 50 --no-pager`
- UI (if it won't start, run from a terminal): `sluice-ui`

```
paste logs here
```

> Security vulnerability? Please **don't** file a public issue — see [SECURITY.md](../../SECURITY.md).
