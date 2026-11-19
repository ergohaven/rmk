# Phenom series

Flash `central` to the physical left half and `peripheral` to the physical right half, using the matching Full, Mini or Micro profile. The binary names retain their historical meaning for flashing; either half can become the USB primary.

Connect one USB data cable. At startup each half waits up to two seconds for USB enumeration. The enumerated half runs the keyboard and host configuration service; the other forwards input over the serial link. A peripheral that receives no primary status retries startup after three seconds. Cable changes that remove power restart this election.

Each half retains its own saved keymap and settings. The current USB primary loads its local configuration and sends live module and lighting settings to the other half. These updates do not copy its saved configuration into the other half's flash.

Physical key positions, left/right encoders and module IDs remain tied to their halves throughout role selection. Reflash both halves when installing this firmware.

To enter ROM USB UF2 mode, press the physical Reset button twice within the 200 ms startup window on the half connected to USB. A normal boot waits an extra 200 ms before USB initialization and role election. See [physical double-Reset](DOUBLE_RESET.md) for the retention/startup contract and focused regression.

The factory base-layer indicator is white. Loading a saved configuration preserves its chosen colors, including an explicitly disabled base-layer indicator; select white in Entropy if you want it for an existing configuration.

Selected touchpads retain fast polling during normal input inactivity and do not enter the sensor's LP1/LP2 modes. Disabling a module still suspends its touchpad, and the shared driver's explicit sleep-event handling is unchanged. This does not add automatic USB host-suspend propagation to the wired Phenom build. K:04 retains its battery-saving policy.
