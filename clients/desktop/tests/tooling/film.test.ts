import { expect, test } from "bun:test";
import { concatList, shots } from "../../scripts/film.ts";

test("frames sort by their millisecond names and keep only the range", () => {
  const names = ["000000900.png", "000000100.png", "film.txt", "000000500.part", "000000300.png"];
  expect(shots(names).map((shot) => shot.at)).toEqual([100, 300, 900]);
  expect(shots(names, 200, 800).map((shot) => shot.at)).toEqual([300]);
});

test("each frame is held until the next one, and the last is repeated", () => {
  const list = concatList("/f", shots(["000000100.png", "000000350.png"]));
  expect(list).toBe(
    [
      "file '/f/000000100.png'",
      "duration 0.250",
      "file '/f/000000350.png'",
      "duration 1.000",
      "file '/f/000000350.png'",
      "",
    ].join("\n"),
  );
});
