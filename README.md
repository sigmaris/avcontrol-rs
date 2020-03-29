# avcontrol-rs

Controls my TV and AV switches over RS232. This runs as a daemon on a Raspberry Pi in my living room with three USB->RS232 adapters to control the HDMI switch, Component switch and TV.

The switching is controlled by an [input_select](https://www.home-assistant.io/integrations/input_select/) widget in [Home Assistant](https://www.home-assistant.io/) which publishes an MQTT message when its state changes; this daemon listens for the MQTT messages and sends commands to the TV and AV switches, and publishes "switched to" MQTT messages with the "retain" flag which indicate the currently switched-to input.

HDMI switch: Ex-Pro AV-Pro V2.0 5-Port HDMI Switch
Component switch: Cables To Go 89028 3x5 Component Video Switch
TV: [Samsung UE46EH5000](https://www.dropbox.com/s/ht3vp4tr943e1ue/Samsung%20TV%20RS232%20Codes%20unlocked.xlsx?dl=0)

```
                                                  +-----------+
                                                  | Dreamcast |
                                                  +-----------+
                                                        |
                                                        v
+-----+  +-----------------+  +---------------+  +-------------+  +-------------------+  +-----+    +-----+
| PS4 |  | Nintendo Switch |  | Fire TV Stick |  |  VGA->HDMI  |  |  Ext. HDMI cable  |  | PS2 |    | Wii |
+-----+  +-----------------+  +---------------+  +-------------+  +-------------------+  +-----+    +-----+
   |              |                   |                 |                   |               |          |
   +-------+------+-------------------+-----------------+-------------------+               +----+-----+
           |                                                                                     |
           v                                                                                     v
    +-------------+          +-------------+          +--------------------+           +------------------+
    | HDMI Switch |          | Kodi (R.Pi) |          | SCART (SNES & N64) |           | Component Switch |
    +-------------+          +-------------+          +--------------------+           +------------------+
              |                     |                            |                           |
              v HDMI1               v HDMI2                      v SCART                     v Component
         +-----------------------------------------------------------------------------------------+
         |                                                                                         |
         |                                        TV                                               |
         |                                                                                         |
         +-----------------------------------------------------------------------------------------+
```
