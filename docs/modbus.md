# Modbus

termvisor speaks Modbus as a **master** (reads and writes a device's
tables) and as a **slave** (simulates a device), over **RTU** (serial
port) or **TCP**. Frames go both ways in the terminal under the tables,
in hex with a time stamp.

## Opening one

Welcome screen → **Modbus**, or New Session → Type **Modbus**:

| Field | Meaning |
|---|---|
| Role | Master (polls a slave) or Slave (answers a master) |
| Link | RTU (serial port) or TCP |
| Serial Port | RTU only: device, baud rate, data bits, parity, stop bits (as for serial sessions) |
| Host, Port | TCP master: the slave's address (port 502). TCP slave: the address to listen on (`0.0.0.0` for all) and the port |
| Unit ID | Master: the unit new polls ask. Slave: the unit it answers; 0 answers any |
| Reply Timeout | Master: how long to wait for an answer (ms) |

On Linux a slave cannot listen on ports below 1024 without extra
rights; use 5020 or similar.

On RTU, DTR and RTS are raised on connect (many RS-485 adapters need
them); ports without modem lines just skip that.

## Master

Each **poll** is a tab: what to read and how often.

| Control | Meaning |
|---|---|
| Unit | Slave id, 1–247 |
| Table | 01 Coils, 02 Discrete Inputs, 03 Holding Registers, 04 Input Registers |
| Address, Count | 0-based start address and how many (up to 250 shown) |
| Every … ms | Scan rate |
| Format | Unsigned, Signed, Hex, Binary, Long AB CD / CD AB, Float AB CD / CD AB (32-bit values take two registers) |
| Plot | Sends this poll's values to the pane's plot (one poll at a time) |
| Scan | Reads at the scan rate when on |

**Add Poll** adds a tab after the last one; the × on a tab removes it.
Each tab shows ok, errors and timeouts, and the last error (exception
codes in words, e.g. "02 illegal data address").

**Writing:** double-click a value. A coil switches (function 05). A
holding register asks for the new value in the poll's format and writes
it with 06 (one register) or 16 (two, for long and float). Writes go
ahead of the reads waiting. Discrete inputs and input registers are
read only.

Only one request is on its way at a time (RS-485 is half duplex). On RTU
the next one waits 3.5 characters after the answer. When the link drops
or a connect fails, it tries again every 2 s.

The polls of a session are remembered by its title in
`~/.local/state/termvisor/modbus.json`.

## Slave

Four tabs, one per table, each showing a range of addresses (Address,
Count) in a chosen format. Every value can be changed, also discrete
inputs and input registers, to simulate a device: type a value and
press Enter, or click a coil.

Requests are answered from these tables at once; a master's writes show
up in them. Unknown functions, bad values and addresses past the end get
exception replies (01, 03, 02). On RTU, unit 0 is a broadcast: writes are
done without a reply. A TCP slave accepts several masters at the same
time.

The values that are not zero and the table views are remembered by the
session title, so a simulated device comes back as it was.

## Functions

| Code | Function |
|---|---|
| 01 | Read coils |
| 02 | Read discrete inputs |
| 03 | Read holding registers |
| 04 | Read input registers |
| 05 | Write single coil |
| 06 | Write single register |
| 15 | Write multiple coils |
| 16 | Write multiple registers |

## Code

| File | Content |
|---|---|
| `src/modbus.rs` | Requests and responses, CRC-16, RTU and TCP framing, frame assembly from a stream, the slave's tables, value formats (with tests) |
| `src/mblink.rs` | Session settings and the link: serial port, TCP connection, TCP server for slaves (with a socket test) |
| `src/modbuspane.rs` | Master and slave panes |
