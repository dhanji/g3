# g3-console

A web-based console for monitoring and managing running g3 instances.

## Features

- **Instance Discovery**: Automatically detects all running g3 processes (both binary and `cargo run`)
- **Real-time Monitoring**: View live statistics, progress, and logs
- **Process Control**: Kill and restart instances
- **Launch New Instances**: Start new g3 runs with custom configuration
- **Project Context**: View requirements, README, and git status
- **Chat History**: Browse complete conversation history with syntax highlighting
- **Tool Call Inspection**: Examine tool calls with parameters and results
- **Dark/Light Themes**: Modern Hero UI design system

## Installation

```bash
# Build the console
cargo build --release -p g3-console

# Or run directly
cargo run --release -p g3-console
```

## Usage

```bash
# Start console on default port (9090)
g3-console

# Specify custom port
g3-console --port 3000

# Specify custom host
g3-console --host 0.0.0.0

# Auto-open browser
g3-console --open
```

## Frontend Development

The frontend is built with React and Vite.

```bash
cd crates/g3-console/web

# Install dependencies
npm install

# Run development server (with hot reload)
npm run dev

# Build for production
npm run build
```

## Architecture

### Backend (Rust)

- **Axum** web framework for REST API
- **Process detection** using `sysinfo` crate
- **Log parsing** from `<workspace>/logs/` directories
- **Process control** via system signals

### Frontend (React)

- **React Router** for navigation
- **Tailwind CSS** for styling
- **Hero UI** design system
- **Marked** for Markdown rendering
- **Highlight.js** for syntax highlighting

## API Endpoints

- `GET /api/instances` - List all running instances
- `GET /api/instances/:id` - Get instance details
- `GET /api/instances/:id/logs` - Get instance logs
- `POST /api/instances/launch` - Launch new instance
- `POST /api/instances/:id/kill` - Kill instance
- `POST /api/instances/:id/restart` - Restart instance

## Configuration

Console state is persisted in `~/.config/g3/console-state.json`.

## Requirements

- Rust 1.70+
- Node.js 18+ (for frontend development)
- Running g3 instances with `--workspace` flag

## License

MIT
