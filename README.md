This is an attempt to provide a rust binding to the Akai Fire controller with
special attention paid to supporting multiple devices.

Status:
- Multiple Devices: Can identify and connect to multiple devices!
- Input: In theory knows how to parse events but who knows!
- Output: Can light up a single button!

Built on top of the post-0.5.0 `midir` development branch.  All understanding of
the protocol is thanks to Paul Curtis' series of blog posts:
- https://blog.segger.com/decoding-the-akai-fire-part-1/
- https://blog.segger.com/decoding-the-akai-fire-part-2/
- https://blog.segger.com/decoding-the-akai-fire-part-3/

Core MIDI parsing informed/inspired by:
- https://github.com/mitchmindtree/novation_remote_25sl
- https://github.com/JoshuaBatty/korg_nano_kontrol_2 (which is inspired by the novation binding)
