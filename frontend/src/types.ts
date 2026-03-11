export interface ParagraphVersion {
  text: string;
  timestamp: number;
}

export interface TrackedParagraph {
  id: string;
  currentText: string;
  history: ParagraphVersion[];
  startLine: number;
  endLine: number;
}

export interface ParsedParagraph {
  startLine: number;
  endLine: number;
  text: string;
}

export interface ChatMessage {
  role: "user" | "assistant" | "system";
  content: string;
}

export interface IModelDeltaDecoration {
  range: {
    startLineNumber: number;
    startColumn: number;
    endLineNumber: number;
    endColumn: number;
  };
  options: Record<string, unknown>;
}
