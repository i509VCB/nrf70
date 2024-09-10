#!/usr/bin/env python3
import re
import glob
import os
import shutil
import subprocess

def confirm():
    text = input("This script is going to clone the updated Nordic repositories (at least a few hundred MB). Enter 'y' to proceed: ")

    if text.lower() != 'y':
        exit(1)

def check_git():
    try:
        subprocess.run(["git"])
    except:
        print("Error: Could not find git")
        exit(1)

def get_firmware_commit() -> str:
    pattern = re.compile("/raw/(.+)/nrf_wifi/fw_bins")

    with open("./target/hal_nordic/zephyr/module.yml", "r") as module:
        match = pattern.search(module.read())

    if not match:
        print("Error: could not find firmware bin commit from modules.yml")
        exit(1)

    return match.group(1)

def clone_headers():
    HEADERS_URL = "https://github.com/zephyrproject-rtos/hal_nordic"

    try:
        shutil.rmtree("./target/hal_nordic")
    except:
        pass

    os.mkdir("./target/hal_nordic")

    subprocess.run(
        [
            "git",
            "clone",
            HEADERS_URL,
            "./target/hal_nordic"
        ],
        check = True
    )

def clone_firmware(commit: str):
    FIRMWARE_URL = "https://github.com/nrfconnect/sdk-nrfxlib/"

    try:
        shutil.rmtree("./target/nrfxlib")
    except:
        pass

    os.mkdir("./target/nrfxlib")

    # Avoid cloning 200MB+ each time
    subprocess.run(
        [
            "git",
            "clone",
            FIRMWARE_URL,
            "--depth",
            "1",
            "./target/nrfxlib"
        ],
        check = True,
    )

    os.chdir("./target/nrfxlib")

    # Then checkout the commit noted in modules.yml
    subprocess.run(
        [
            "git",
            "fetch",
            "--depth",
            "1",
            "origin",
            commit,
        ],
        check = True,
    )

    subprocess.run(
        [
            "git",
            "checkout",
            "FETCH_HEAD",
        ],
        check = True,
    )

    # Restore the current dir after updating the repo with firmware.
    os.chdir("../..")

def bindgen_fw_if():
    subprocess.run(
        [
            "bindgen",
            "gen_wrapper.h",
            "--output=fw/bindings.rs",
            "--use-core",
            "--ignore-functions",
            "--default-enum-style=rust",
            "--no-prepend-enum-name",
            "--no-layout-tests",
            "--",
            "-I./target/hal_nordic/drivers/nrf_wifi/fw_if/umac_if/inc/fw/",
            "-I./target/hal_nordic/drivers/nrf_wifi/hw_if/hal/inc/fw/",
        ],
        check = True
    )

# check_git()
# confirm()
# clone_headers()
# commit = get_firmware_commit()
# clone_firmware(commit)
bindgen_fw_if()

# # for f in glob.glob("fw/*.bin"):
# #     os.remove(f)

h = open("fw/bindings.rs").read()
h = re.sub("= (\d+);", lambda m: "= 0x{:x};".format(int(m[1])), h)
h = h.replace("pub enum", "#[derive(num_enum::TryFromPrimitive)] pub enum")
h = h.replace("NRF_WIFI_802", "IEEE_802")
h = h.replace("NRF_WIFI_", "")
h = h.replace("nrf_wifi_", "")
h = h.replace("nrf70_", "")
h = h.replace("NRF70_", "")
open("fw/bindings.rs", "w").write(h)

subprocess.run(
    [
        "rustfmt",
        "--edition=2021",
        "fw/bindings.rs",
    ],
    check=True,
)

# h = open(
#     "fw/fw_if/umac_if/inc/fw/rpu_fw_patches.h"
# ).read()

# flavors = {}
# flavors["_radiotest"] = re.search(
#     re.compile("#ifdef CONFIG_NRF700X_RADIO_TEST(.*)#else", re.MULTILINE | re.DOTALL), h
# )[1]
# flavors[""] = re.search(re.compile("#else(.*)#endif", re.MULTILINE | re.DOTALL), h)[1]

# for suffix, code in flavors.items():
#     for fw in re.findall(
#         re.compile(
#             "const unsigned char __aligned\\(4\\)\\s+([a-z0-9_]+)\\[\\] = \\{([a-f0-9x, \t\r\n]+)\\}",
#             re.MULTILINE,
#         ),
#         code,
#     ):
#         name = fw[0].removeprefix("wifi_nrf_") + suffix + ".bin"
#         data = bytes.fromhex(
#             "".join(c for c in fw[1].replace("0x", "") if c in "0123456789abcdef")
#         )
#         open("fw/" + name, "wb").write(data)
