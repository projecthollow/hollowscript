// The small, read-only subset of the canonical WASM wrapper used by the
// member worker. esbuild injects the implementation from its original source.
declare const HollowScriptTooling: {
  fromBytes(bytes: Uint8Array, options: { maxMemoryBytes: number }): Promise<{
    analyse(source: string, entryName: string, capabilities: unknown[], modules: Record<string, string>): {
      documentSymbols(): { kind: string; name: string }[];
      completions(offset: number): { label: string; kind: string; detail: string }[];
      diagnostics(): { code: string; message: string; start: number; end: number }[];
      dispose(): void;
    };
  }>;
};
