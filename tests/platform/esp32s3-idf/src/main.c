#include "esp_log.h"
#include "sdkconfig.h"
#include "probe.h"

#if CONFIG_LOG_DEFAULT_LEVEL != 2
#error "sdkconfig_options must reach the IDF component build"
#endif

void app_main(void) {
    ESP_LOGW("fbuild-idf", "%s", idf_probe_message());
}
