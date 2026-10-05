import fixture from "./profile.json" with { type: "json" };
import config from "./config.json" with { type: "json" };

export function createSession() {
  let negotiated = false;
  return function respond(raw) {
    let request;
    const error = (id, code, message) => ({ jsonrpc: "2.0", id, error: { code, message } });
    try { request = JSON.parse(raw.toString()); }
    catch { return error(null, -32700, "Parse error"); }
    if (!request || request.jsonrpc !== "2.0" || typeof request.method !== "string" ||
        (request.id !== undefined && typeof request.id !== "string" && typeof request.id !== "number")) {
      return error(null, -32600, "Invalid request");
    }
    if (request.id === undefined) return null;
    const { id, method, params = {} } = request;
    if (!params || typeof params !== "object" || Array.isArray(params)) return error(id, -32602, "Invalid params");
    if (method === "hello") {
      negotiated = typeof params.proto === "string" && params.proto.split(".")[0] === config.protocol.split(".")[0];
      return { jsonrpc: "2.0", id, result: { core: config.server, proto: config.protocol, compatible: negotiated } };
    }
    if (!negotiated) return error(id, -32001, "Compatible hello required");
    let result;
    switch (method) {
      case "profile.list": result = fixture.profiles.map(({ id, name, project_id }) => ({ id, name, project_id })); break;
      case "profile.get":
        result = fixture.profiles.find((profile) => profile.id === params.id);
        if (!result) return error(id, -32602, "Unknown profile");
        break;
      case "module.list": result = fixture.modules; break;
      case "execution.list": result = fixture.executions; break;
      default: return error(id, -32601, "Method not found");
    }
    return { jsonrpc: "2.0", id, result };
  };
}
