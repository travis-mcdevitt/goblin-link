# generic-sdl

Desktop development and CI platform. It is the detection fallback and declares only a 1024x768 display and `/tmp`; every other capability
(LEDs, rumble, battery, volume, mic gain, autostart, firmware registration, brightness, stay-awake) is absent and reports "unavailable", which is
the behaviour a UI should handle by hiding the control. `goblinctl platform info` on a development host selects it.
