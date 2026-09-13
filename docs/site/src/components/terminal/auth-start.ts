export type AuthProvider = "github" | "google";
export type AuthReturnPath = "/" | "/embed";

export function nativeAuthStartUrl(
  authOrigin: string,
  provider: AuthProvider,
  returnPath: AuthReturnPath = "/embed",
): string {
  const url = new URL(`/auth/${provider}`, authOrigin);
  url.searchParams.set("return_to", returnPath);
  return url.toString();
}
