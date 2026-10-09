# /// script
# dependencies = ["pychromecast"]
# ///
"""Launch the tinderbox Cast receiver on a Chromecast.

uv run tools/cast-launch.py "Living Room TV"     # friendly name from the Home app
"""
import sys
import time

import pychromecast

APP_ID = "A7739EC5"

name = sys.argv[1]
chromecasts, browser = pychromecast.get_listed_chromecasts(friendly_names=[name])
if not chromecasts:
    sys.exit(f"no Chromecast named {name!r} found on the network")
cast = chromecasts[0]
cast.wait()
cast.start_app(APP_ID)
time.sleep(2)
print(f"launched {APP_ID} on {name}: {cast.status.display_name}")
browser.stop_discovery()
