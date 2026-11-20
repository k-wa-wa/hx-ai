import { useEffect, useRef } from "react";
import { DoneAction, RestoreTarget } from "../api";

export interface ChatMessage {
  id: number;
  role: "user" | "assistant" | "system" | "error";
  text: string;
  changes?: DoneAction[];
  notRestored?: RestoreTarget[];
  meta?: string;
}

export function ChatLog({ messages, progress }: { messages: ChatMessage[]; progress: string | null }) {
  const end = useRef<HTMLDivElement>(null);
  useEffect(() => {
    end.current?.scrollIntoView({ behavior: "smooth" });
  }, [messages, progress]);

  return (
    <div className="chat">
      {messages.map((m) => (
        <div key={m.id} className={`msg ${m.role}`}>
          {m.text && <div className="msg-text">{m.text}</div>}
          {m.changes && m.changes.length > 0 && (
            <ul className="changes">
              {m.changes.map((c, i) => (
                <li key={i}>
                  <span className="param">
                    {c.block} / {c.parameter}
                  </span>
                  <span className="vals">
                    {c.previous_value} → <b>{c.value}</b>
                  </span>
                </li>
              ))}
            </ul>
          )}
          {m.notRestored && m.notRestored.length > 0 && (
            <ul className="changes warn">
              {m.notRestored.map((t, i) => (
                <li key={i}>
                  <span className="param">
                    {t.block} / {t.parameter}
                  </span>
                  <span className="vals">{t.value} に戻せていない</span>
                </li>
              ))}
            </ul>
          )}
          {m.meta && <div className="meta">{m.meta}</div>}
        </div>
      ))}
      {progress && (
        <div className="msg progress">
          <span className="spinner" /> {progress}
        </div>
      )}
      <div ref={end} />
    </div>
  );
}
