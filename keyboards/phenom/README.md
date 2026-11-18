# Phenom series

Flash `central` to the physical left half and `peripheral` to the physical right half, using the matching Full, Mini or Micro profile. The binary names retain their historical meaning for flashing; either half can become the USB primary.

Connect one USB data cable. At startup each half waits up to two seconds for USB enumeration. The enumerated half runs the keyboard and host configuration service; the other forwards input over the serial link. A peripheral that receives no primary status retries startup after three seconds. Cable changes that remove power restart this election.

Each half retains its own saved keymap and settings. The current USB primary loads its local configuration and sends live module and lighting settings to the other half. These updates do not copy its saved configuration into the other half's flash.

Physical key positions, left/right encoders and module IDs remain tied to their halves throughout role selection. Reflash both halves when installing this firmware.
