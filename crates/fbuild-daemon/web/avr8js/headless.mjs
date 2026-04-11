// Headless avr8js runner — spawned by fbuild daemon as a Node.js subprocess.
// UART output is written to stdout; the Rust side reads it line-by-line
// through the same MonitorState pipeline used for QEMU.
//
// Usage: node headless.mjs --hex <firmware.hex> [--f-cpu <hz>]

import { readFileSync } from "node:fs";
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
} from "avr8js";

// ---------------------------------------------------------------------------
// CLI argument parsing
// ---------------------------------------------------------------------------

function parseArgs(argv) {
  const args = { fCpuHz: 16_000_000, hexPath: null };
  for (let i = 2; i < argv.length; i++) {
    if (argv[i] === "--hex" && argv[i + 1]) {
      args.hexPath = argv[++i];
    } else if (argv[i] === "--f-cpu" && argv[i + 1]) {
      args.fCpuHz = Number(argv[++i]);
    }
  }
  if (!args.hexPath) {
    process.stderr.write("error: --hex <path> is required\n");
    process.exit(1);
  }
  return args;
}

// ---------------------------------------------------------------------------
// Intel HEX parser (same logic as app.js)
// ---------------------------------------------------------------------------

function parseIntelHex(hexText) {
  let upperAddress = 0;
  const bytes = [];
  let maxAddress = 0;

  for (const rawLine of hexText.split(/\r?\n/)) {
    const line = rawLine.trim();
    if (!line) continue;
    if (!line.startsWith(":")) {
      throw new Error(`invalid Intel HEX line: ${line}`);
    }

    const byteCount = parseInt(line.slice(1, 3), 16);
    const address = parseInt(line.slice(3, 7), 16);
    const recordType = parseInt(line.slice(7, 9), 16);
    const data = line.slice(9, 9 + byteCount * 2);

    if (recordType === 0x00) {
      const base = upperAddress + address;
      for (let i = 0; i < byteCount; i++) {
        bytes[base + i] = parseInt(data.slice(i * 2, i * 2 + 2), 16);
      }
      maxAddress = Math.max(maxAddress, base + byteCount);
    } else if (recordType === 0x01) {
      break;
    } else if (recordType === 0x04) {
      upperAddress = parseInt(data, 16) << 16;
    }
  }

  const programBytes = new Uint8Array(maxAddress);
  for (let i = 0; i < maxAddress; i++) {
    programBytes[i] = bytes[i] ?? 0;
  }

  const programWords = new Uint16Array(Math.ceil(programBytes.length / 2));
  for (let i = 0; i < programBytes.length; i += 2) {
    programWords[i >> 1] = programBytes[i] | ((programBytes[i + 1] ?? 0) << 8);
  }
  return programWords;
}

// ---------------------------------------------------------------------------
// Simulator setup
// ---------------------------------------------------------------------------

function createSimulator(program, fCpuHz) {
  const cpu = new CPU(program, 2048); // ATmega328P: 2 KB SRAM

  new AVRIOPort(cpu, portBConfig);
  new AVRIOPort(cpu, portCConfig);
  new AVRIOPort(cpu, portDConfig);
  new AVRTimer(cpu, timer0Config);
  new AVRTimer(cpu, timer1Config);
  new AVRTimer(cpu, timer2Config);

  const usart = new AVRUSART(cpu, usart0Config, fCpuHz);
  usart.onLineTransmit = (line) => {
    process.stdout.write(line.replace(/\r+$/, "") + "\n");
  };

  return cpu;
}

// ---------------------------------------------------------------------------
// Main execution loop
// ---------------------------------------------------------------------------

function runLoop(cpu) {
  const batch = 50_000;

  function tick() {
    for (let i = 0; i < batch; i++) {
      avrInstruction(cpu);
      cpu.tick();
    }
    setImmediate(tick);
  }

  setImmediate(tick);
}

// ---------------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------------

const args = parseArgs(process.argv);
const hexText = readFileSync(args.hexPath, "utf-8");
const program = parseIntelHex(hexText);

process.stderr.write(
  `[avr8js] headless: loaded ${program.length} words, f_cpu=${args.fCpuHz}\n`
);

const cpu = createSimulator(program, args.fCpuHz);
runLoop(cpu);

// Graceful shutdown
process.on("SIGTERM", () => process.exit(0));
process.on("SIGINT", () => process.exit(0));
