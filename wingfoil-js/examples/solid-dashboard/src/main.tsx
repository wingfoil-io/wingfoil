/** Solid dashboard demo for @wingfoil/client. */
import { createSignal, onCleanup } from "solid-js";
import { render } from "solid-js/web";
import { WingfoilClient } from "@wingfoil/client";
import { useTopic, usePublisher } from "@wingfoil/client/solid";

interface PriceTick {
  mid: number;
  count: number;
}

function App() {
  const client = new WingfoilClient({
    url: import.meta.env.VITE_WINGFOIL_URL ?? "ws://127.0.0.1:8080/ws",
  });
  onCleanup(() => client.close());

  const [status, setStatus] = createSignal<string>("connecting");
  client.onConnection((state) => {
    switch (state.kind) {
      case "connecting":
        setStatus("connecting…");
        break;
      case "open":
        setStatus(`open (codec=${state.codec}, version=${state.version})`);
        break;
      case "closed":
        setStatus(`closed (${state.code})`);
        break;
      case "error":
        setStatus("error");
        break;
    }
  });

  const price = useTopic<PriceTick>(client, "price");
  const sendUi = usePublisher<{ kind: string; note: string }>(client, "ui");

  return (
    <div class="wrap">
      <h1>wingfoil live price</h1>
      <div class="card">
        <div class="meta">status: {status()}</div>
        <div class="mid">{price()?.mid.toFixed(4) ?? "—"}</div>
        <div class="meta">tick #{price()?.count ?? 0}</div>
      </div>
      <div class="card">
        <button onClick={() => sendUi({ kind: "click", note: "hello" })}>
          send "click" event to graph
        </button>
      </div>
    </div>
  );
}

render(() => <App />, document.getElementById("app")!);
