import {
  CPU,
  avrInstruction,
  AVRIOPort,
  portBConfig,
  portCConfig,
  portDConfig,
  AVRTimer,
  timer0Config,
  timer1Config,
  timer2Config,
  AVRUSART,
  usart0Config,
} from "https://esm.sh/avr8js@0.21.0";

const stdoutEl = document.getElementById("stdout");
const statusEl = document.getElementById("status");
const sessionId = window.__AVR8JS_SESSION_ID__;

function setStatus(message) {
  if (statusEl) {
    statusEl.textContent = message;
  }
}

function appendOutput(line) {
  if (!stdoutEl) {
    return;
  }
  stdoutEl.value += `${String(line).replace(/\r+$/, "")}\n`;
  stdoutEl.scrollTop = stdoutEl.scrollHeight;
}

function parseIntelHex(hexText) {
  let upperAddress = 0;
  const bytes = [];
  let maxAddress = 0;

  for (const rawLine of hexText.split(/\r?\n/)) {
    const line = rawLine.trim();
    if (!line) {
      continue;
    }
    if (!line.startsWith(":")) {
      throw new Error(`invalid Intel HEX line: ${line}`);
    }

    const byteCount = Number.parseInt(line.slice(1, 3), 16);
    const address = Number.parseInt(line.slice(3, 7), 16);
    const recordType = Number.parseInt(line.slice(7, 9), 16);
    const data = line.slice(9, 9 + byteCount * 2);

    if (recordType === 0x00) {
      const base = upperAddress + address;
      for (let i = 0; i < byteCount; i += 1) {
        bytes[base + i] = Number.parseInt(data.slice(i * 2, i * 2 + 2), 16);
      }
      maxAddress = Math.max(maxAddress, base + byteCount);
    } else if (recordType === 0x01) {
      break;
    } else if (recordType === 0x04) {
      upperAddress = Number.parseInt(data, 16) << 16;
    }
  }

  const programBytes = new Uint8Array(maxAddress);
  for (let i = 0; i < maxAddress; i += 1) {
    programBytes[i] = bytes[i] ?? 0;
  }

  const programWords = new Uint16Array(Math.ceil(programBytes.length / 2));
  for (let i = 0; i < programBytes.length; i += 2) {
    programWords[i >> 1] = programBytes[i] | ((programBytes[i + 1] ?? 0) << 8);
  }
  return programWords;
}

async function loadSession() {
  if (!sessionId) {
    throw new Error("missing AVR8js session id");
  }
  const sessionResp = await fetch(`/api/emulator/avr8js/${sessionId}`);
  if (!sessionResp.ok) {
    throw new Error(`failed to load session: ${sessionResp.status}`);
  }
  const session = await sessionResp.json();

  const firmwareResp = await fetch(session.firmware_hex_url);
  if (!firmwareResp.ok) {
    throw new Error(`failed to load firmware.hex: ${firmwareResp.status}`);
  }
  const firmwareHex = await firmwareResp.text();
  return { session, firmwareHex };
}

function createSimulator(session, firmwareHex) {
  const program = parseIntelHex(firmwareHex);
  const cpu = new CPU(program, 2048);

  new AVRIOPort(cpu, portBConfig);
  new AVRIOPort(cpu, portCConfig);
  new AVRIOPort(cpu, portDConfig);
  new AVRTimer(cpu, timer0Config);
  new AVRTimer(cpu, timer1Config);
  new AVRTimer(cpu, timer2Config);

  const usart = new AVRUSART(cpu, usart0Config, session.f_cpu_hz || 16000000);
  usart.onLineTransmit = (line) => appendOutput(line);

  return cpu;
}

function startCpu(cpu) {
  const instructionBatch = 5000;
  const frameBudgetMs = 12;

  function runFrame() {
    const frameStart = performance.now();
    while (performance.now() - frameStart < frameBudgetMs) {
      for (let i = 0; i < instructionBatch; i += 1) {
        avrInstruction(cpu);
        cpu.tick();
      }
    }
    requestAnimationFrame(runFrame);
  }

  requestAnimationFrame(runFrame);
}

async function main() {
  setStatus("Loading firmware...");
  const { session, firmwareHex } = await loadSession();
  setStatus(`Running ${session.board_id} (${session.mcu})`);
  appendOutput(`[avr8js] session ${session.session_id}`);
  appendOutput(`[avr8js] waiting for Serial output...`);
  const cpu = createSimulator(session, firmwareHex);
  startCpu(cpu);
}

main().catch((error) => {
  setStatus("Startup failed");
  appendOutput(`[avr8js] ${error.message}`);
  console.error(error);
});
