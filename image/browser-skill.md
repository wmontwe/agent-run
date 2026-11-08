---
name: browser
description: Look at web pages and GUI apps you build. Load, click through and screenshot pages in a headless Chromium with the `browser` command, or screenshot desktop apps (Compose Desktop) on the virtual display. Use it to check what a change looks like.
---

# Browser and screenshots

Web pages: `browser` drives one headless Chromium (1280x800) that keeps
running between calls. Serve the page yourself (e.g. `./gradlew kobwebStart`
or another dev server, in the background) and open it on `localhost`.

```sh
browser open http://localhost:8080      # prints title, URL, console output
browser screenshot /tmp/page.png        # --full for the whole page
browser click 'text=Sign in'            # Playwright selectors
browser type '#email' 'a@example.com'
browser press Enter
browser wait '.result'
browser text main                       # visible text of an element
browser eval 'document.title'
browser console                         # console output and errors so far
browser stop
```

Desktop apps: they run on the virtual display (`DISPLAY=:99`). Start the app
in the background, wait for its window, then capture the screen:

```sh
./gradlew run > /tmp/app.log 2>&1 &
sleep 20 && scrot /tmp/screen.png
```

Look at a screenshot by reading the PNG with the `read` tool. Only localhost
and the hosts on the proxy's allowlist are reachable.
