---
created_date: "2026-09-29"
document_status: final
closed_date: "2026-09-29"
---

# Test plan — settings-compat-shell-surfaces

## Manual procedures

### Wallpaper renders behind the live session

Automated tests cover the drop file, decode, cover-crop, and
fallback paths, but no unit test owns a GL framebuffer, so the final
visual needs human eyes once per release.

1. In the Roost session, set a wallpaper:
   `gsettings set org.gnome.desktop.background picture-uri
   'file:///usr/share/backgrounds/gnome/adwaita-l.jpg'`
   (any local image works).
2. Run the nested session: `scripts/roost-journey --artifacts
   /tmp/wp-check` (needs the CI journey prerequisites).
3. Open `/tmp/wp-check` frames: the background behind windows shows
   the image, cover-cropped with no bars. With the recovery overlay
   visible (kill the shell child), the background stays solid red.
4. Unset the key or point it at a missing file and re-run: the
   background falls back to the solid dark clear, and the session
   stays up.

Pass: image visible in step 3, solid fallback in step 4, no
compositor errors on stderr. Who/when: release check, run by the
person cutting the release.
