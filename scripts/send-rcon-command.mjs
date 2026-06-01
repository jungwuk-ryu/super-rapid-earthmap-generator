import net from "node:net";

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

const host = valueOf("--host", "127.0.0.1");
const port = Number.parseInt(valueOf("--port", "25575"), 10);
const password = valueOf("--password");
const command = valueOf("--command");
const timeoutMs = Number.parseInt(valueOf("--timeout-ms", "30000"), 10);

const AUTH = 3;
const COMMAND = 2;

let nextRequestId = 1000;

function packet(type, body) {
  const requestId = nextRequestId++;
  const bodyBytes = Buffer.from(body, "utf8");
  const length = 4 + 4 + bodyBytes.length + 2;
  const buffer = Buffer.alloc(4 + length);
  buffer.writeInt32LE(length, 0);
  buffer.writeInt32LE(requestId, 4);
  buffer.writeInt32LE(type, 8);
  bodyBytes.copy(buffer, 12);
  buffer.writeInt8(0, 12 + bodyBytes.length);
  buffer.writeInt8(0, 13 + bodyBytes.length);
  return { requestId, buffer };
}

class PacketReader {
  constructor(socket) {
    this.socket = socket;
    this.buffer = Buffer.alloc(0);
    this.pending = [];
    this.waiting = [];
    this.closed = false;
    this.socket.on("data", (chunk) => this.onData(chunk));
    this.socket.once("close", () => {
      this.closed = true;
      this.rejectWaiting(new Error("RCON socket closed before a response arrived"));
    });
    this.socket.once("error", (error) => {
      this.rejectWaiting(error);
    });
  }

  onData(chunk) {
    this.buffer = Buffer.concat([this.buffer, chunk]);
    while (this.buffer.length >= 4) {
      const length = this.buffer.readInt32LE(0);
      if (this.buffer.length < 4 + length) {
        break;
      }
      const packetBuffer = this.buffer.subarray(4, 4 + length);
      this.buffer = this.buffer.subarray(4 + length);
      this.pending.push({
        id: packetBuffer.readInt32LE(0),
        type: packetBuffer.readInt32LE(4),
        body: packetBuffer.subarray(8, packetBuffer.length - 2).toString("utf8"),
      });
    }
    this.flushWaiting();
  }

  flushWaiting() {
    while (this.pending.length > 0 && this.waiting.length > 0) {
      const packetResponse = this.pending.shift();
      const waiting = this.waiting.shift();
      waiting.resolve(packetResponse);
    }
  }

  rejectWaiting(error) {
    while (this.waiting.length > 0) {
      this.waiting.shift().reject(error);
    }
  }

  readPacket() {
    if (this.pending.length > 0) {
      return Promise.resolve(this.pending.shift());
    }
    if (this.closed) {
      return Promise.reject(new Error("RCON socket closed before a response arrived"));
    }
    return new Promise((resolve, reject) => {
      this.waiting.push({ resolve, reject });
    });
  }
}

async function readResponseFor(reader, requestId, description) {
  for (;;) {
    const response = await reader.readPacket();
    if (response.id === -1) {
      throw new Error(`${description} failed`);
    }
    if (response.id === requestId) {
      return response;
    }
  }
}

const socket = net.createConnection({ host, port });
socket.setTimeout(timeoutMs);
socket.on("timeout", () => {
  socket.destroy(new Error(`RCON timeout after ${timeoutMs} ms`));
});

await new Promise((resolve, reject) => {
  socket.once("connect", resolve);
  socket.once("error", reject);
});

const reader = new PacketReader(socket);

const authPacket = packet(AUTH, password);
socket.write(authPacket.buffer);
await readResponseFor(reader, authPacket.requestId, "RCON authentication");

const commandPacket = packet(COMMAND, command);
socket.write(commandPacket.buffer);
const commandResponse = await readResponseFor(reader, commandPacket.requestId, `RCON command "${command}"`);
if (commandResponse.body.length > 0) {
  console.log(commandResponse.body);
}
socket.end();
