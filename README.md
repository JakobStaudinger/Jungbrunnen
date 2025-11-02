# Jungbrunnen
An embedded project using a Raspberry Pi Pico to strobe LEDs to make it look like water droplets are floating in mid-air.
Heavily inspired by [isaac879's RGB Time Fountain](https://github.com/isaac879/RGB-Time-Fountain)

> [!WARNING]
> 🚧 Under development 🚧

## Firmware
Firmware for the cyw43 chip can be found [here](https://github.com/embassy-rs/embassy/tree/main/cyw43-firmware). It should be placed in the `cyw43-firmware` folder to make the program compile.

### Dev Firmware
To reduce the time it takes to flash the program to the Pico, you can enable the `dev_firmware` feature. This makes it so the firmware doesn't get embedded into the binary, and instead the program assumes it is already located at a fixed memory address.

You need to ensure the firmware is actually pre-baked into the flash memory. To do this, use the `probe-rs download` command, like so:
```console
$ probe-rs download cyw43-firmware/43439A0.bin --binary-format bin --chip RP2040 --base-address 0x10100000
$ probe-rs download cyw43-firmware/43439A0_clm.bin --binary-format bin --chip RP2040 --base-address 0x10140000
```
