#ifndef YUNYIN_YP_IO_H
#define YUNYIN_YP_IO_H

#ifdef __cplusplus
extern "C" {
#endif

/*
 * 解码器的输入抽象（任务书 §32）。
 *
 * Phase 1 只做一件事：把"喂给解码器的字节从哪来"从"文件路径"换成这组回调。
 * 六个解码循环本身一行不动（§35），换的只是打开时用的那套 API。
 *
 * 关键规则（§32/§15）：
 *   - read() == 0 只代表**真正结束**，不能拿来表示"暂时没数据"；
 *   - 因此"数据够不够"由上层（Gate）判断，IO 回调自己不等待、不重试；
 *   - 网络实现（Phase 2）在缓存不足时会让 read() 阻塞到补上数据为止，
 *     绝不让解码器看见一个假的 EOF。
 *
 * size() 允许返回负数表示"长度未知"：这种情况下 seek 到文件尾不可用，
 * 调用方需要靠 duration hint 或解码器自己报的时长（§37）。
 */

typedef struct yp_io {
    void *ctx;

    /* 读最多 size 字节；返回实际读到的字节数，0 = 真正结束，负数 = 出错 */
    long long (*read)(void *ctx, void *dst, unsigned long long size);

    /* whence 用 SEEK_SET / SEEK_CUR / SEEK_END；返回新的绝对位置，负数 = 失败 */
    long long (*seek)(void *ctx, long long offset, int whence);

    /* 当前绝对位置 */
    long long (*tell)(void *ctx);

    /* 总长度；负数 = 未知 */
    long long (*size)(void *ctx);

    /* 关闭并释放 ctx（只有拥有者会调；见 yp_open_spec 的 owns_io） */
    int (*close)(void *ctx);
} yp_io;

/*
 * 用文件系统实现一份（本地播放用）。成功返回 0。
 * 网络侧不共用它 —— Phase 2 的 HttpRangeSource 会自己实现这组回调。
 */
int yp_io_file_open(yp_io *io, const char *path);

#ifdef __cplusplus
}
#endif

#endif /* YUNYIN_YP_IO_H */
