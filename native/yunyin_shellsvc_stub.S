@ 云音：libShellAudio 要用 sceShellSvcGetSvcObj()（拿 shell 的音频服务对象），
@ 但 vitasdk 的 SceShellSvc stub 只导出了 SceShellUtil* 那几个。这里按 SDK
@ 自己的 stub 目标文件格式，手写一个 NID 导入（vita-elf-create 会把它并进
@ 模块的导入表）：
@   库 NID 0x773242C7 = SceShellSvc
@   函数 NID 0xB31E7F1C = sceShellSvcGetSvcObj
@ 格式与 libSceShellSvc_stub.a 里各成员的 .vitalink.fstubs.* 段一致：
@   .word 0 / 库 NID / 函数 NID / nop

	.section .vitalink.fstubs.SceShellSvc,"ax",%progbits
	.align 4
	.global sceShellSvcGetSvcObj
	.type sceShellSvcGetSvcObj, %function
sceShellSvcGetSvcObj:
	.word 0x00000000
	.word 0x773242C7
	.word 0xB31E7F1C
	.word 0xe320f000

@ 让 shell 进入/退出 BGM 模式（0 = disable，1 = enable BackGround Music）。
@   库 NID 0xD2B1C8AE = SceShellUtil
@   函数 NID 0xB65B60CA = sceShellUtilSetBGMMode

	.section .vitalink.fstubs.SceShellUtil,"ax",%progbits
	.align 4
	.global sceShellUtilSetBGMMode
	.type sceShellUtilSetBGMMode, %function
sceShellUtilSetBGMMode:
	.word 0x00000000
	.word 0xD2B1C8AE
	.word 0xB65B60CA
	.word 0xe320f000
