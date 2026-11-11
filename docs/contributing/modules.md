# Write a Regulus module

A module supplies nodes behind a small interface. Choose a concrete workflow that
needs the behavior before adding machinery to the core. Read the
[concepts](concepts.md) and [contribution guide](../../CONTRIBUTING.md) first.

## Choose the extension path

Use a template when existing nodes already implement the behavior. For a new native
capability, create a Rust crate under crates/modules and register it by hand: there is no
install command (`regulus module install` is an approved design that has not been built),
and the crate is only in the `regulus` binary after its line in the CLI's module registry
and a rebuild. For a project-local extension in another language, write a local module
that speaks the ProcessModule protocol: `regulus node init` (Python, with the one-file SDK
in [sdk/python](../../sdk/python/)) or `regulus node init --lang rust` (with the helper in
[sdk/rust](../../sdk/rust/)) and `regulus node build`; see also the
[Python echo example](../../examples/local-modules/echo/) and the
[Rust stdio example](../../examples/local-modules/rust-stdio/). Local processes run only
in trusted projects, reload without restarting the core (`regulus project reload`) and
only get `emit` from ModuleCtx; check the protocol before choosing this route.

## Native module checklist

1. Create the crate and its regulus-module.toml. Follow an existing small module such
   as [hello](../../crates/modules/hello/), rather than copying a large agent engine.
2. Declare node types, versioned capability ports and configurable fields. The client
   draws fields from the manifest. Prefer optional inputs; justify required inputs.
3. Implement Module: manifest, init, call and shutdown. Handle node/start, node/event
   and node/stop when your nodes need lifecycle or event processing.
4. Register the crate in the workspace and the explicit CLI module registry. The core
   must not import your module or contain a branch based on its id.
5. Add an example and focused tests, then verify startup with your module removed
   through the ablation matrix. Exercise the feature through CLI before claiming UI
   support. Add client handling only where generic manifest rendering is insufficient.

ModuleCtx resolves connected endpoints, calls capabilities, emits values and gives a
module its own persistent state. Use that interface rather than accessing another
module's private files or database namespace. Keep blocking work off the bus thread.
Stop owned processes and threads when the node stops. Report failures as observable
errors and test missing dependencies, cancellation and restart where relevant.

Capabilities describe an interface, not the identity of a particular implementation.
An incompatible payload change needs a new capability version. Optional additive
fields must have defaults so existing implementations keep working. Keep contracts
in crates/contracts when multiple implementations share them.

If adapting external code, preserve its license and attribution and record its
origin in the module manifest. A module must remain removable after adaptation.

The full contract and current ProcessModule messages live in
[docs/03-modules.md](../03-modules.md); lifecycle and bus behavior are in
[docs/02-architecture.md](../02-architecture.md). Consult those canonical documents
when their detail exceeds this introductory guide.
