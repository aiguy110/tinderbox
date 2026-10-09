# /// script
# dependencies = ["pychromecast"]
# ///
"""Launch the tinderbox Cast receiver on a Chromecast.

uv run tools/cast-launch.py "Living Room TV"     # find it by friendly name (mDNS, same subnet only)
uv run tools/cast-launch.py --host 192.168.10.187  # or connect straight to its IP

The Chromecast is on the guest network (192.168.10.187, static lease) and mDNS doesn't cross
zones, so use --host; the router has a rule letting bifrost reach it on 8008/8009/8443.
"""
import sys
import time

import pychromecast

APP_ID = "A7739EC5"

if sys.argv[1] == "--host":
    host = sys.argv[2]
    cast = pychromecast.get_chromecast_from_host((host, 8009, None, None, host))
    browser = None
    name = host
else:
    name = sys.argv[1]
    chromecasts, browser = pychromecast.get_listed_chromecasts(friendly_names=[name])
    if not chromecasts:
        sys.exit(f"no Chromecast named {name!r} found on the network")
    cast = chromecasts[0]
cast.wait()
cast.start_app(APP_ID)
time.sleep(2)
print(f"launched {APP_ID} on {name}: {cast.status.display_name}")
if browser:
    browser.stop_discovery()
cast.disconnect()
