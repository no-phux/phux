import { mergeProps } from "@gpuix/solid";

export function LostReactivity(props: { title: string }) {
  const merged = mergeProps({ title: "Default" }, props);
  const title = merged.title;
  return <text>{title}</text>;
}
