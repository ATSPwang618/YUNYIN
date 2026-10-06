import { Text, View } from "@pocketjs/framework/components";
import { pTxt, APP_VERSION, APP_VER_SFO, POCKETJS_VERSION } from "../core/theme";
import { psLockInfo } from "../core/ui-state";

/* 关于页（子页）：静态文字 + PS 键锁状态。 */

export function AboutPage() {
  return (
    <View class="grow w-full flex-col items-center justify-center gap-1 overflow-hidden">
      <Text class={pTxt("aboutTitle")}>YUNYIN 云音 for vita</Text>
      <Text class={pTxt("aboutSub")}>VER {APP_VERSION}  ·  APP {APP_VER_SFO}</Text>
      <Text class={pTxt("aboutSub")}>PocketJS {POCKETJS_VERSION}</Text>
      <Text class={pTxt("aboutSub")}>made by 阡陌</Text>
      <Text class={pTxt("aboutSub")}>致谢：PocketJS 团队</Text>
      <Text class={pTxt("aboutSub")}>播放后端参考 ElevenMPV-A</Text>
      <Text
        class={
          psLockInfo() !== "PS KEY  UNLOCKED"
            ? pTxt("aboutTitle")
            : pTxt("aboutSub")
        }
      >
        {psLockInfo()}
      </Text>
    </View>
  );
}
