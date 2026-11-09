# Regulus concepts

Regulus hosts modules for building and operating systems of AI agents. Its core
provides generic composition and execution machinery. AI behavior belongs to modules.

| Term | Meaning |
|---|---|
| Module | A package that supplies node types and declares the capabilities it offers. |
| Node | An instance of a module's node type, configured inside a workflow. |
| Resource | A file such as a skill or prompt, used by a configured system. |
| Uses wire | A lasting dependency on a capability, such as an engine or toolset. |
| Then wire | A typed value delivered from an output to a connected input. |
| Workflow | Nodes, ports and wires describing a system; workflows can form boxes. |
| Profile | Saved workflows and configuration for a project or global use. |
| Template | A reusable profile with declared variables and module requirements. |
| Execution | A running copy of a workflow; its events are observable from clients. |
| Project | A folder with regulus.toml and optional local profiles, templates and modules. |

Composition is where you edit the design. Operate is where you inspect running
executions, events, conversations and decisions. The desktop app, CLI and TUI talk
to the same core; the desktop app must not be the only way to use a capability.

An Agent node is a box around an engine such as Claude Code or opencode. Providers,
skills, tools and hooks connect by capability. A provider slot for a CLI's internal
subagents is different from a Regulus-managed worker execution.

A liquid box has a stable external contract and replaceable internals. Composition
should let users choose dependencies and save their own configuration as a template.
Removing a module must leave unrelated modules working; required dependencies may
disable a node with an explanation. The core must never special-case module names.

This is a short English entry point, not a second specification. For the detailed
model and current implementation limits see [composition](../11-composition.md),
[architecture](../02-architecture.md) and [the current plan](../plan.md). Some documents
are design decisions rather than claims that every feature is already implemented.
