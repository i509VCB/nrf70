// The nrf-sdk expects these macros to be defined by either Zephyr or Linux.
// We are not either so these must be defined.
#define __packed __attribute__((__packed__))
#define __aligned(x) __attribute__((__aligned__(x)))

#include "host_rpu_common_if.h"
#include "host_rpu_data_if.h"
#include "host_rpu_sys_if.h"
#include "host_rpu_umac_if.h"
#include "lmac_if_common.h"
#include "patch_info.h"

#include "phy_rf_params.h"
#include "rpu_if.h"
