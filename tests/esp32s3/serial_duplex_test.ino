// Serial Duplex Test Sketch for ESP32-S3
//
// This sketch implements a simple bidirectional serial communication test
// to help diagnose serial port locking issues. It uses a command-response
// pattern similar to FastLED's RPC-JSON protocol but simplified.
//
// PROTOCOL:
//   Host sends: {"cmd":"<command>","data":"<optional_data>"}\n
//   Device responds: {"status":"ok","cmd":"<command>","response":"<response>"}\n
//
// SUPPORTED COMMANDS:
//   - ping: Simple echo test
//   - led_on: Turn built-in LED on
//   - led_off: Turn built-in LED off
//   - blink: Blink LED once
//   - echo: Echo back the provided data
//   - info: Get device information
//
// USAGE:
//   1. Upload this sketch to ESP32-S3
//   2. Open serial monitor at 115200 baud
//   3. Send JSON commands (one per line)
//   4. Device will respond with JSON
//
// Example session:
//   > {"cmd":"ping"}
//   < {"status":"ok","cmd":"ping","response":"pong"}
//   > {"cmd":"echo","data":"hello"}
//   < {"status":"ok","cmd":"echo","response":"hello"}
//   > {"cmd":"led_on"}
//   < {"status":"ok","cmd":"led_on","response":"LED is ON"}
//
// This sketch is designed to NOT monopolize the serial port with constant
// output, only responding when commanded. This helps test whether full-duplex
// communication causes port locking issues.

#define LED_PIN 48         // Built-in LED on ESP32-S3-DevKitC-1
#define BAUD_RATE 115200
#define MAX_CMD_LENGTH 256

// Command buffer
char commandBuffer[MAX_CMD_LENGTH];
int bufferIndex = 0;
bool ledState = false;

void setup() {
  // Initialize serial communication
  Serial.begin(BAUD_RATE);

  // Wait for serial connection (or timeout after 3 seconds)
  unsigned long startTime = millis();
  while (!Serial && (millis() - startTime < 3000)) {
    delay(10);
  }

  // Initialize LED pin
  pinMode(LED_PIN, OUTPUT);
  digitalWrite(LED_PIN, LOW);

  // Send startup message
  Serial.println("{\"status\":\"ready\",\"device\":\"ESP32-S3\",\"protocol\":\"simple-json\"}");
  Serial.println("# ESP32-S3 Serial Duplex Test Ready");
  Serial.println("# Send JSON commands (e.g., {\"cmd\":\"ping\"})");
  Serial.flush();
}

void loop() {
  // Check if data is available on serial
  while (Serial.available() > 0) {
    char c = Serial.read();

    // Handle newline (end of command)
    if (c == '\n' || c == '\r') {
      if (bufferIndex > 0) {
        commandBuffer[bufferIndex] = '\0';
        processCommand(commandBuffer);
        bufferIndex = 0;
      }
    }
    // Add character to buffer
    else if (bufferIndex < MAX_CMD_LENGTH - 1) {
      commandBuffer[bufferIndex++] = c;
    }
    // Buffer overflow - reset
    else {
      sendError("command_too_long");
      bufferIndex = 0;
    }
  }

  // Small delay to prevent CPU spinning
  delay(1);
}

// Parse and execute command
void processCommand(const char* cmdLine) {
  // Simple JSON parsing (look for "cmd":"<value>")
  const char* cmdStart = strstr(cmdLine, "\"cmd\"");
  if (!cmdStart) {
    sendError("missing_cmd_field");
    return;
  }

  // Find the command value
  const char* valueStart = strchr(cmdStart, ':');
  if (!valueStart) {
    sendError("malformed_cmd");
    return;
  }
  valueStart++; // Skip ':'

  // Skip whitespace and opening quote
  while (*valueStart == ' ' || *valueStart == '"') valueStart++;

  // Extract command name
  char cmd[32] = {0};
  int i = 0;
  while (*valueStart && *valueStart != '"' && *valueStart != ',' && i < 31) {
    cmd[i++] = *valueStart++;
  }
  cmd[i] = '\0';

  // Extract optional data field
  char data[128] = {0};
  const char* dataStart = strstr(cmdLine, "\"data\"");
  if (dataStart) {
    const char* dataValueStart = strchr(dataStart, ':');
    if (dataValueStart) {
      dataValueStart++;
      while (*dataValueStart == ' ' || *dataValueStart == '"') dataValueStart++;
      int j = 0;
      while (*dataValueStart && *dataValueStart != '"' && *dataValueStart != '}' && j < 127) {
        data[j++] = *dataValueStart++;
      }
      data[j] = '\0';
    }
  }

  // Execute command
  executeCommand(cmd, data);
}

// Execute the parsed command
void executeCommand(const char* cmd, const char* data) {
  // PING command
  if (strcmp(cmd, "ping") == 0) {
    sendResponse(cmd, "pong");
  }

  // LED ON command
  else if (strcmp(cmd, "led_on") == 0) {
    digitalWrite(LED_PIN, HIGH);
    ledState = true;
    sendResponse(cmd, "LED is ON");
  }

  // LED OFF command
  else if (strcmp(cmd, "led_off") == 0) {
    digitalWrite(LED_PIN, LOW);
    ledState = false;
    sendResponse(cmd, "LED is OFF");
  }

  // BLINK command
  else if (strcmp(cmd, "blink") == 0) {
    digitalWrite(LED_PIN, HIGH);
    delay(100);
    digitalWrite(LED_PIN, LOW);
    delay(100);
    sendResponse(cmd, "blinked");
  }

  // ECHO command
  else if (strcmp(cmd, "echo") == 0) {
    if (strlen(data) > 0) {
      sendResponse(cmd, data);
    } else {
      sendResponse(cmd, "no data to echo");
    }
  }

  // INFO command
  else if (strcmp(cmd, "info") == 0) {
    char info[128];
    snprintf(info, sizeof(info), "ESP32-S3 @ %lu MHz, LED=%s",
             ESP.getCpuFreqMHz(), ledState ? "ON" : "OFF");
    sendResponse(cmd, info);
  }

  // LED TOGGLE command
  else if (strcmp(cmd, "toggle") == 0) {
    ledState = !ledState;
    digitalWrite(LED_PIN, ledState ? HIGH : LOW);
    sendResponse(cmd, ledState ? "LED toggled ON" : "LED toggled OFF");
  }

  // Unknown command
  else {
    sendError("unknown_command", cmd);
  }
}

// Send success response
void sendResponse(const char* cmd, const char* response) {
  Serial.print("{\"status\":\"ok\",\"cmd\":\"");
  Serial.print(cmd);
  Serial.print("\",\"response\":\"");
  Serial.print(response);
  Serial.println("\"}");
  Serial.flush();
}

// Send error response
void sendError(const char* errorType) {
  Serial.print("{\"status\":\"error\",\"error\":\"");
  Serial.print(errorType);
  Serial.println("\"}");
  Serial.flush();
}

void sendError(const char* errorType, const char* details) {
  Serial.print("{\"status\":\"error\",\"error\":\"");
  Serial.print(errorType);
  Serial.print("\",\"details\":\"");
  Serial.print(details);
  Serial.println("\"}");
  Serial.flush();
}
