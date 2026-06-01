import { spawn } from "node:child_process";
import { createWriteStream, mkdirSync, readFileSync } from "node:fs";
import { dirname, resolve } from "node:path";

const args = process.argv.slice(2);

function valueOf(name, fallback = undefined) {
  const index = args.indexOf(name);
  if (index < 0) {
    if (fallback !== undefined) {
      return fallback;
    }
    throw new Error(`Missing ${name}`);
  }
  if (index + 1 >= args.length) {
    throw new Error(`Missing value for ${name}`);
  }
  return args[index + 1];
}

const serverRoot = resolve(valueOf("--server-root"));
const serverJar = resolve(valueOf("--server-jar"));
const logPath = resolve(valueOf("--log"));
const commandFile = resolve(valueOf("--commands"));
const startupTimeoutMs = Number.parseInt(valueOf("--startup-timeout-ms", "240000"), 10);
const shutdownTimeoutMs = Number.parseInt(valueOf("--shutdown-timeout-ms", "120000"), 10);
const commandLines = readFileSync(commandFile, "utf8")
  .split(/\r?\n/)
  .map((line) => line.trim())
  .filter((line) => line.length > 0 && !line.startsWith("#"));

const instructions = [];
const requiredLogMarkers = [];
for (const line of commandLines) {
  const waitMatch = line.match(/^@wait-ms\s+([0-9]+)$/);
  if (waitMatch) {
    instructions.push({ type: "wait", milliseconds: Number.parseInt(waitMatch[1], 10) });
    continue;
  }
  const requireLogMatch = line.match(/^@require-log\s+(.+)$/);
  if (requireLogMatch) {
    const marker = requireLogMatch[1];
    requiredLogMarkers.push(marker);
    instructions.push({ type: "require-log", marker });
    continue;
  }
  if (line.startsWith("@")) {
    throw new Error(`Unknown command directive: ${line}`);
  }
  instructions.push({ type: "command", command: line });
}

mkdirSync(dirname(logPath), { recursive: true });
const log = createWriteStream(logPath, { flags: "w", encoding: "utf8" });

let started = false;
let failure = false;
let stopping = false;
let settled = false;
let startupTimer;
let shutdownTimer;
const seenRequiredLogMarkers = new Set();

function write(line) {
  log.write(line.endsWith("\n") ? line : `${line}\n`);
}

function finish(code) {
  if (settled) {
    return;
  }
  settled = true;
  clearTimeout(startupTimer);
  clearTimeout(shutdownTimer);
  log.end(() => process.exit(code));
}

const javaArgs = ["-Xms512M", "-Xmx1536M", "--add-modules=jdk.incubator.vector", "-jar", serverJar, "nogui"];
write(`[codex-runner] cwd=${serverRoot}`);
write(`[codex-runner] java ${javaArgs.join(" ")}`);
write(`[codex-runner] commands=${commandFile}`);

const child = spawn("java", javaArgs, {
  cwd: serverRoot,
  stdio: ["pipe", "pipe", "pipe"],
  windowsHide: true,
});

function delay(ms) {
  return new Promise((resolveDelay) => {
    setTimeout(resolveDelay, ms);
  });
}

async function sendCommandsAndStop() {
  if (stopping) {
    return;
  }
  stopping = true;
  for (const instruction of instructions) {
    if (instruction.type === "wait") {
      write(`[codex-runner] wait ${instruction.milliseconds} ms`);
      await delay(instruction.milliseconds);
      continue;
    }
    if (instruction.type === "require-log") {
      write(`[codex-runner] require log marker: ${instruction.marker}`);
      continue;
    }
    const command = instruction.command;
    write(`[codex-runner] > ${command}`);
    child.stdin.write(`${command}\n`);
  }
  write("[codex-runner] > stop");
  child.stdin.write("stop\n");
  shutdownTimer = setTimeout(() => {
    write("[codex-runner] Shutdown timeout; killing process.");
    child.kill("SIGKILL");
    finish(1);
  }, shutdownTimeoutMs);
}

function handleLine(line) {
  write(line);
  for (const marker of requiredLogMarkers) {
    if (!seenRequiredLogMarkers.has(marker) && line.includes(marker)) {
      seenRequiredLogMarkers.add(marker);
    }
  }
  if (line.includes("Done (")) {
    started = true;
    setTimeout(sendCommandsAndStop, 750);
  }
  if (/Encountered an unexpected exception|Failed to start|FAILED TO BIND|Exception loading|Could not load|World files may be corrupted/i.test(line)) {
    failure = true;
    if (!stopping) {
      child.kill("SIGKILL");
    }
  }
}

let stdoutBuffer = "";
let stderrBuffer = "";
function consume(buffer, chunk) {
  buffer += chunk.toString("utf8");
  const lines = buffer.split(/\r?\n/);
  const rest = lines.pop() ?? "";
  for (const line of lines) {
    if (line.length > 0) {
      handleLine(line);
    }
  }
  return rest;
}

child.stdout.on("data", (chunk) => {
  stdoutBuffer = consume(stdoutBuffer, chunk);
});
child.stderr.on("data", (chunk) => {
  stderrBuffer = consume(stderrBuffer, chunk);
});
child.on("error", (error) => {
  write(`[codex-runner] Failed to spawn server: ${error.stack ?? error.message}`);
  finish(1);
});
child.on("exit", (code, signal) => {
  if (stdoutBuffer.length > 0) {
    handleLine(stdoutBuffer);
  }
  if (stderrBuffer.length > 0) {
    handleLine(stderrBuffer);
  }
  write(`[codex-runner] Process exited code=${code} signal=${signal ?? ""}`);
  const missingRequiredLogMarkers = requiredLogMarkers.filter((marker) => !seenRequiredLogMarkers.has(marker));
  for (const marker of missingRequiredLogMarkers) {
    write(`[codex-runner] Missing required log marker: ${marker}`);
  }
  if (missingRequiredLogMarkers.length > 0) {
    failure = true;
  }
  if (started && !failure && code === 0) {
    write("[codex-runner] Server command run completed cleanly.");
    finish(0);
  } else {
    finish(1);
  }
});

startupTimer = setTimeout(() => {
  if (!started) {
    write("[codex-runner] Startup timeout; killing process.");
    child.kill("SIGKILL");
    finish(1);
  }
}, startupTimeoutMs);
