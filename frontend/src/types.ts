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

export interface ApiErrorBody {
  error: string;
  current_revision?: string | null;
  deleted?: boolean;
  conflicting_path?: string;
  conflicting_url?: string;
  overlapping_paths?: string[];
  local_paths?: string[];
  remote_paths?: string[];
  unpushed?: boolean;
  repo_blocked?: string;
  files?: GitChange[];
  subject?: string;
}

export interface ArchivePost {
  path: string;
  title: string;
  date: string | null;
  draft: boolean;
  unsorted: boolean;
  url: string;
  revision: string;
}

export interface PostsResponse { posts: ArchivePost[]; }

export interface PostResponse {
  path: string;
  content: string;
  revision: string;
  url: string;
  title: string;
}

export interface SaveResponse { revision: string; url: string; }

export interface CreatePostResponse extends SaveResponse {
  path: string;
  content: string;
}

export interface RenamePreviewResponse {
  old_url: string;
  new_url: string;
  url_changes: boolean;
}

export interface RenamePostResponse { path: string; url: string; }
export interface RecoverPostResponse extends RenamePostResponse { revision: string; }

export type GitChangeKind = "added" | "modified" | "deleted" | "renamed";
export interface GitChange { path: string; kind: GitChangeKind; }

export interface GitStatusResponse {
  changes: GitChange[];
  unpushed: boolean;
  repo_blocked: "merge" | "rebase" | "cherry-pick" | null;
}

export interface GitPrepareResponse {
  files: GitChange[];
  subject: string;
  behind: boolean;
}

export interface GitPublishResponse {
  status: "pushed" | "push_failed";
  commit: string;
  error?: string;
}

export interface GitSyncResponse { updated: boolean; }

export interface PostDocumentController {
  initializeSelection(): Promise<void>;
  openPost(path: string, supplied?: PostResponse): Promise<boolean>;
  flush(path?: string): Promise<boolean>;
  isDirty(path: string): boolean;
  getKnownRevision(path: string): string | null;
  getActivePath(): string | null;
  renameDocument(oldPath: string, newPath: string, url: string): void;
  disposeDocument(path: string): void;
  clearSelection(openArchive?: boolean): void;
  checkActiveRevision(): Promise<void>;
  setArchiveOpener(opener: () => void): void;
}
