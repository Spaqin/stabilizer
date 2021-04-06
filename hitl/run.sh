#!/usr/bin/bash

# Title:
#   Stabilizer hardware-in-the-loop (HITL) test script.
#
# Description:
#   This shell file is executed by the hardware runner in Quartiq's office to exercise the various
#   hardware aspects of Stabilizer.

# Test pinging Stabilizer. This exercises that:
# * DHCP is functional and an IP has been acquired
# * Stabilizer's network is functioning as intended
# * The stabilizer application is opeerational
ping -c 5 -W 20 gonnigan.ber.quartiq.de

# Test the MQTT interface.
python3 miniconf.py dt/sinara/stabilizer afe/0 '"G2"'
