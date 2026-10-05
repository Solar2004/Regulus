import { WebSocketServer } from "ws";
import { createSession } from "./rpc.mjs";
import config from "./config.json" with { type: "json" };

export function compositionMock() {
  return {
    name: "regulus-composition-mock",
    configureServer(server) {
      const wss = new WebSocketServer({ noServer: true, maxPayload: config.maxPayload });
      const upgrade = (request, socket, head) => {
        if (request.url?.split("?")[0] !== config.path) return;
        const origin = request.headers.origin;
        if (origin && new URL(origin).host !== request.headers.host) { socket.destroy(); return; }
        wss.handleUpgrade(request, socket, head, (ws) => {
          const respond = createSession();
          ws.on("message", (message) => {
            const response = respond(message);
            if (response) ws.send(JSON.stringify(response));
          });
        });
      };
      server.httpServer.on("upgrade", upgrade);
      server.httpServer.once("close", () => {
        server.httpServer.off("upgrade", upgrade);
        for (const client of wss.clients) client.terminate();
        wss.close();
      });
    },
  };
}
