<!-- Status: non-normative design note. Normative semantic, wire, and ownership
     sources are linked below and in docs/README.md. -->
# C ABI and JDK SPI Design Note（C ABI 与 JDK SPI 设计说明）

> This document is a non-normative design note, not a current public API
> commitment. The semantic contract and repository boundary documents are
> authoritative.
>
> 本文是非规范设计说明，不是当前公开 API 承诺。语义契约与仓库边界文档才是
> 权威来源。

可以。现在最值得做的不是马上写 `libcy.so` 和一堆 Kotlin class，而是先把它们当成**两个受约束的投影层**来规定。

我会建议你把规则定成：

> **Semantic Contract 决定“是什么意思”；Wire Protocol 决定“怎么传”；C ABI 和 JVM SPI 决定“各语言怎么舒服地使用”。**

这样 C ABI 和 JVM SPI 可以分别演进，而不会反过来控制内核设计。

---

# 一、先定四层，不要混在一起

```text
                Cy Semantic Contract
              最稳定：系统真正的宪法
                       │
       Worker / Operation / Resource
       Lease / Capability / Endpoint
       Provider / Principal / Event
                       │
            ┌──────────┴──────────┐
            ▼                     ▼
      Wire Contract            API Bindings
      UDS / schema             C ABI / JVM SPI
                                  │
                    ┌─────────────┴────────────┐
                    ▼                          ▼
                 Native                     JVM
             C/C++/Rust/C#             Java/Kotlin/Scala
```

四个版本号也**不要绑定**：

```text
Semantic Contract v1
Wire Protocol       v3
C ABI               v2
JVM SPI             v1
```

以后你可以升级 Wire，而完全不改变 Semantic Contract。

---

# 二、C ABI：我建议做得比你现在想象的更小

我甚至不会一开始就暴露几十个：

```c
cy_resource_acquire_lease(...)
cy_worker_create(...)
cy_operation_submit(...)
cy_endpoint_open(...)
...
```

而是分成两层。

## C ABI Core：真正冻结

只有大概 8～12 个函数。

概念类似：

```c
cy_api_get(...)
cy_session_open(...)
cy_session_close(...)

cy_call(...)
cy_call_async(...)

cy_event_poll(...)
cy_cancel(...)

cy_buffer_data(...)
cy_buffer_size(...)
cy_buffer_release(...)
```

其中核心其实是：

```c
cy_call(
    session,
    request_type,
    request_bytes,
    request_len,
    result
)
```

也就是说：

> C ABI 本身不知道 ResourceAcquireRequest 的 C struct 长什么样。

它只传**Canonical Wire Message**。

---

# 这会产生一个非常重要的优势

以后 Semantic Contract 增加：

```text
ResourceQuery v2
```

你不用：

```text
修改 libcy ABI
重新定义 C struct
担心 struct layout
担心 C#/Rust/Go binding
```

只是：

```text
新的 message schema
```

C ABI：

> **完全不变。**

所以我会把：

```text
libcy.so
```

看成一个非常薄的：

> **Native Cy Protocol Client**

而不是“大型 native SDK”。

---

# 三、然后在 C ABI Core 上提供 Typed C SDK

开发者当然不想天天手写 bytes。

所以再提供：

```text
cy-native-sdk
```

里面可以有：

```c
cy_resource_acquire(...)
cy_operation_submit(...)
cy_worker_query(...)
cy_endpoint_open(...)
```

但这些只是：

```text
typed wrapper
     ↓
encode canonical message
     ↓
cy_call()
```

于是：

```text
                Stable C ABI Core
                      ▲
                      │
          ┌───────────┼───────────┐
          │           │           │
      C typed SDK   C# binding  Rust SDK
          │           │           │
          └────── encode/decode ──┘
                      │
                    cy_call
```

**逻辑：**

真正冻结的是底下几个函数。

上面的 ergonomic API 可以逐步改善。

这能显著降低未来 ABI 债务。

---

# 四、C ABI 的硬性规定，我会直接写进项目宪法

## 1. ABI 只能使用 C ABI

Rust：

```rust
extern "C"
```

不能暴露 Rust ABI。

Rust 官方的 FFI 文档本身也是围绕 `extern "C"`、opaque struct 等模式来描述跨语言边界的。

禁止跨边界暴露：

```text
Rust String
Vec<T>
Option<T>
Result<T,E>

std::string
std::vector
C++ class
template
exception
```

---

## 2. 对象全部 opaque

例如：

```c
typedef struct cy_session cy_session_t;
typedef struct cy_buffer cy_buffer_t;
```

外部不知道：

```text
sizeof(cy_session)
里面有什么
是不是 Rust struct
有没有 socket
```

只能：

```c
cy_session_open(...)
cy_session_close(...)
```

这给你未来重写 native client 的自由。

---

# 3. Semantic ID 用固定表示

例如我会认真考虑：

```c
typedef struct {
    uint8_t bytes[16];
} cy_id_t;
```

而不是：

```c
long
void*
size_t
```

来表达系统身份。

于是：

```text
WorkerId
ResourceId
OperationId
LeaseId
EndpointId
```

都可以统一成为：

```text
cy_id_t
```

类型安全 wrapper 可以由上层语言提供。

---

# 4. 不让调用方释放你的内存

基本原则：

> **谁分配，谁释放。**

例如：

```c
cy_buffer_t* result;

cy_call(..., &result);

const uint8_t* p = cy_buffer_data(result);

cy_buffer_release(result);
```

不要：

```text
Rust 分配
↓
C# free()

C malloc()
↓
Rust drop()
```

.NET 官方 native interop 指南同样强调签名和原生类型必须匹配，并建议使用受控 handle 管理 unmanaged resource 生命周期。

---

# 5. 不让 panic / exception 穿 ABI

规定：

```text
Rust panic
C++ exception
JVM exception
```

绝对不能穿过 C ABI。

ABI 世界永远返回：

```c
cy_status_t
```

比如：

```text
CY_OK
CY_INVALID_ARGUMENT
CY_NOT_FOUND
CY_PERMISSION_DENIED
CY_CONFLICT
CY_UNAVAILABLE
CY_RESOURCE_EXHAUSTED
CY_DEADLINE_EXCEEDED
CY_CANCELLED
CY_INTERNAL
```

然后额外返回：

```text
diagnostic bytes
```

---

# 6. C ABI 不使用大量 callback

我建议默认：

```text
poll
wait
stream handle
```

而不是：

```text
register_callback_1()
register_callback_2()
...
```

因为 callback 一跨：

```text
Rust ↔ C ↔ CLR/JVM
```

生命周期会明显复杂。

.NET 官方 native interop 指南也专门讨论了 unmanaged callback / function pointer 的生命周期问题。

所以：

```c
cy_event_poll(...)
```

会比复杂 callback 模型稳定很多。

---

# 七、一个值得认真采用的 C ABI 版本模型

入口：

```c
cy_status_t cy_api_get(
    uint32_t abi_version,
    cy_api_t** api
);
```

然后：

```c
struct cy_api_v1 {
    uint32_t struct_size;
    uint32_t abi_version;

    cy_status_t (*session_open)(...);
    void (*session_close)(...);

    cy_status_t (*call)(...);
    cy_status_t (*call_async)(...);

    cy_status_t (*event_poll)(...);
    cy_status_t (*cancel)(...);

    ...
};
```

未来：

```text
CyApiV1
CyApiV2
```

而不是偷偷修改 V1。

`struct_size` 还能让你在某些情况下在末尾追加可选 entry，并进行 feature detection。

---

# 八、因此我认为 C ABI 的核心哲学应该是

> **Small, opaque, message-oriented, ownership-explicit, versioned.**

不是：

> “把整个 Cy Object Model 翻译成 C struct。”

后者非常容易成为永久包袱。

---

# 九、JVM SPI 则正好相反：应该强调类型和可读性

JVM SPI 我甚至建议：

# **直接用 Java 写。**

不是因为 Java 比 Kotlin 好。

而是因为：

```text
cy-jvm-spi
```

应该是：

> **最保守、最无聊、最普适的 JVM 边界。**

它最好：

```text
零 Spring 依赖
零 Kotlin runtime 依赖
尽量只有 JDK dependency
```

于是：

```text
Java
Kotlin
Scala
Clojure
Groovy
```

全部天然能实现。

然后 Kotlin SDK 再包一层。

---

# 十、包结构甚至可以显式版本化

例如：

```text
io.cyplatform.spi.v1
```

里面只有稳定类型。

这样十年后真出现不可调和的设计错误：

```text
io.cyplatform.spi.v2
```

可以和 V1 并存。

而不是硬着头皮给 V1 打二十年补丁。

---

# 十一、JVM SPI 不要暴露九个 Manager

我会采用：

```java
public interface CyClient {
    Resources resources();
    Operations operations();
    Workers workers();
    Endpoints endpoints();
    Events events();
}
```

然后例如：

```java
public interface Resources {

    CompletionStage<ResourceSet> find(
        ResourceQuery query
    );

    CompletionStage<Lease> acquire(
        AcquireRequest request
    );

    CompletionStage<Void> release(
        LeaseId lease
    );
}
```

Operation：

```java
public interface Operations {

    CompletionStage<Operation> submit(
        OperationRequest request
    );

    CompletionStage<Operation> get(
        OperationId id
    );

    CompletionStage<Void> cancel(
        OperationId id
    );
}
```

这样 Spring/JVM 开发者基本不用学习新哲学。

---

# 十二、异步边界：SPI 用 Java 标准类型，SDK 再 Kotlin 化

这里我会很明确：

### JVM SPI

用：

```text
CompletionStage<T>
Flow.Publisher<T>
```

而不要：

```text
suspend
kotlinx.coroutines.Flow
```

因为 SPI 是 JVM contract。

### Kotlin SDK

再包装：

```kotlin
suspend fun acquire(...): Lease

fun events(...): Flow<Event>
```

于是：

```text
                    Semantic async operation
                              │
                ┌─────────────┴─────────────┐
                ↓                           ↓
        JVM SPI                       Kotlin SDK
 CompletionStage<T>                    suspend
 Flow.Publisher<T>                      Flow<T>
```

实现语义一致。

---

# 十三、SPI 的 DTO 不建议用 Kotlin `data class`

尤其是长期稳定 SPI。

因为今天：

```kotlin
data class ResourceQuery(
    val memory: Long
)
```

明天：

```kotlin
data class ResourceQuery(
    val memory: Long,
    val topology: String?
)
```

看起来只是加了字段，但构造器和编译后的 ABI 都会受到影响。

Kotlin 官方库作者指南明确区分 **binary、source、behavioral compatibility**，并指出它们可能彼此独立地被破坏；官方也提供 binary compatibility validation 工具来检查公共 API。

所以长期 SPI 我更推荐：

```java
public final class ResourceQuery {

    private final OptionalLong minimumMemory;
    private final Set<CapabilityId> requiredCapabilities;

    private ResourceQuery(Builder builder) {
        ...
    }

    public OptionalLong minimumMemory() { ... }

    public Set<CapabilityId> requiredCapabilities() { ... }

    public static Builder builder() { ... }
}
```

以后新增：

```java
builder.topology(...)
```

老二进制基本不用动。

---

# 十四、不要把可扩展概念写成 Java enum

这个非常重要。

不要：

```java
enum Capability {
    CUDA,
    ROCM,
    BF16
}
```

应该：

```java
public final class CapabilityId {

    private final String value;

}
```

例如：

```text
ai.accelerator.compute
ai.numeric.bf16

vendor.nvidia.cuda
vendor.amd.rocm
```

未来出现：

```text
vendor.foo.quantum.matrix
```

旧 JVM SPI：

> 完全能承载。

---

# 十五、enum 只用于真正“宪法级封闭集合”

甚至这里我也会非常保守。

比如：

```text
Operation terminal / non-terminal
```

可能相对稳定。

但是：

```text
GPU type
Provider type
Resource class
Capability
Operation kind
```

全部不要 enum。

用：

```text
ResourceClass
CapabilityId
OperationKind
```

这样的 string/value object。

原则：

> **如果你无法证明这个集合十年后仍然封闭，就不要用 enum。**

---

# 十六、Provider SPI 要拆成小接口

不要：

```java
public interface Provider {

    start();
    stop();

    resources();
    execute();
    cancel();
    endpoint();
    reconcile();
    health();
    ...
}
```

我会设计：

```java
public interface CyProvider {
    ProviderDescriptor descriptor();
}
```

再组合：

```java
public interface ResourceProvider extends CyProvider {
    ...
}

public interface OperationProvider extends CyProvider {
    ...
}

public interface EndpointProvider extends CyProvider {
    ...
}

public interface Reconciler extends CyProvider {
    ...
}
```

于是：

```text
NvidiaProvider
├── ResourceProvider
└── Reconciler
```

Python runtime：

```text
PythonProvider
├── OperationProvider
├── EndpointProvider
└── Reconciler
```

未来新增能力：

> 新增小 SPI。

不要修改 `CyProvider` 的基础定义。

---

# 十七、这是避免 Java SPI 锁死最重要的技巧之一

Java interface 最大的风险之一就是：

今天：

```java
interface Provider {
    start();
}
```

明天加入：

```java
execute();
```

所有第三方 implementor 都受到影响。

虽然 Java 可以使用 `default` method 缓和一部分演进问题，但我不会把它当成长期架构逃生门。

更好的办法是：

```text
Provider
     │
     ├── OptionalCapabilityA
     ├── OptionalCapabilityB
     └── OptionalCapabilityC
```

也就是：

> **增加接口，不扩大基础接口。**

---

# 十八、Provider discovery 可以采用 Java 原生机制，但不要绑死

Java 当前仍然提供 `ServiceLoader` 作为标准 service-provider discovery 机制，可以通过 service interface 查找 provider，也支持 module `uses/provides` 模型。

所以可以支持：

```text
META-INF/services
```

或者：

```java
provides CyProvider with NvidiaProvider;
```

但我的建议是：

> `ServiceLoader` 只是一个 discovery adapter。

Cy 自己真正认可的是：

```text
ProviderDescriptor
```

这样未来：

```text
ServiceLoader
plugin directory
remote provider
container provider
```

都能接。

---

# 十九、JVM SPI 的错误也必须和 C ABI 完全对齐

C：

```text
CY_RESOURCE_EXHAUSTED
```

JVM：

```java
CyException {
    ErrorCode code();
    Diagnostic diagnostic();
}
```

其中：

```text
ErrorCode.RESOURCE_EXHAUSTED
```

表达相同 Semantic Error。

Kotlin：

```text
CyException.ResourceExhausted
```

可以再做漂亮包装。

但是本质是同一个错误。

---

# 二十、最重要的是让两套 API 一一映射

例如：

### Semantic Contract

```text
Resource.Acquire
```

### Wire

```text
RESOURCE_ACQUIRE
```

### C

```c
cy_resource_acquire(...)
```

或者底层：

```c
cy_call(... CY_MSG_ACQUIRE_LEASE ...)
```

### Java

```java
cy.resources().acquireLease(...)
```

### Kotlin

```kotlin
cy.resources.acquireLease(...)
```

不要发生：

```text
C       → reserveDevice
Java    → allocateResource
Wire    → CREATE_LEASE
```

虽然技术上能工作，但认知成本会上升。

---

# 二十一、我甚至建议维护一张正式的 Contract Matrix

比如：

| Semantic | Wire | C Typed SDK | JVM SPI |
|---|---|---|---|
| Acquire Lease | `AcquireLease` | `cy_resource_acquire_lease` | `resources.acquireLease()` |
| Release Lease | `ReleaseLease` | `cy_lease_release_lease` | `leases.releaseLease()` |
| Submit Operation | `operation.submit` | `cy_operation_submit` | `operations.submit()` |
| Cancel Operation | `operation.cancel` | `cy_operation_cancel` | `operations.cancel()` |
| Publish Endpoint | `endpoint.publish` | `cy_endpoint_publish` | `endpoints.publish()` |
| Watch Events | `WatchEvents` | `cy_event_watch` | `events.watchEvents()` |
| Reconcile | `provider.reconcile` | provider SDK | `Reconciler.reconcile()` |

这个表最好就是架构仓库的一部分。

---

# 二十二、双方共同遵守的“请求信封”

这一点我认为应该直接进入 Semantic Contract。

任何 control-plane request 都拥有：

```text
RequestContext
├── RequestId
├── Principal
├── Deadline
├── TraceContext
├── IdempotencyKey?
└── Extensions
```

于是 C：

```c
cy_request_context_t
```

Java：

```java
RequestContext
```

Wire：

```text
RequestEnvelope
```

都是一回事。

这以后对于：

```text
超时
重试
分布式 tracing
去重
故障恢复
审计
```

会非常重要。

---

# 二十三、Extension 也必须从第一版存在

比如：

```text
Extension {
    namespace
    version
    content_type
    payload
}
```

以后 NVIDIA 要传：

```text
vendor.nvidia.cuda.graph
```

可以直接：

```text
extensions[]
```

Kernel：

> 不理解。

但：

```text
能携带
能鉴权
能路由
能记录
```

于是你不会因为某厂商突然增加一种特殊能力：

> 修改 C ABI + JVM SPI + Kernel。

---

# 最后，我会把二者规范压缩成两张表

## C ABI 宪法

| 要求 | 规定 |
|---|---|
| ABI | `extern "C"` |
| 对象 | opaque handle |
| 数据 | canonical wire message |
| 内存 | creator frees |
| 错误 | status code + diagnostic |
| 异步 | operation handle / poll |
| Event | poll/stream，不大量 callback |
| 类型 | fixed-width |
| Vendor extension | opaque namespaced payload |
| Version | `cy_api_get(version)` |
| Rust/C++ exception | 禁止跨 ABI |
| 内核知识 | **零实现细节** |

---

## JVM SPI 宪法

| 要求 | 规定 |
|---|---|
| 实现语言 | **建议 Java** |
| dependency | JDK only |
| Kotlin dependency | 无 |
| Spring dependency | 无 |
| 异步 | `CompletionStage` |
| stream | `Flow.Publisher` |
| DTO | immutable + builder |
| extensible identifier | value object，不用 enum |
| Provider | 小接口组合 |
| discovery | ServiceLoader 可选 |
| error | stable semantic error code |
| version | artifact/package major version |
| Kotlin ergonomics | 放 `cy-kotlin-sdk` |
| Spring ergonomics | 放 `cy-spring` |

Kotlin 官方自己的库设计指南也强调公共 API 要降低 mental complexity，并保持清晰、一致、可预测；同时兼容性需要分别考虑 binary/source/behavioral 三个层次。

---

# 如果让我现在给你的仓库分模块

我会倾向最终出现：

```text
cy-kernel-api
cy-kernel-daemon

cy-wire-schema
cy-wire-client

cy-native-abi
cy-native-sdk

cy-jvm-spi
cy-jvm-client
cy-kotlin-sdk
cy-spring

cy-provider-sdk
```

其中真正应该**极端谨慎修改**的是：

```text
cy-kernel-api
cy-wire-schema 的核心语义
cy-native-abi
cy-jvm-spi
```

而：

```text
cy-kotlin-sdk
cy-spring
各种 provider
```

可以比较积极地演进。

---

**结论：**

你的 C ABI 最好不是“把 Cy 的全部类型翻译成 C”，而是一层**非常小的 opaque message ABI**；JVM SPI 则应该是一套**Java 写成、JDK-only、接口组合式、长期二进制稳定的类型化契约**。

两边最终必须满足一个非常强的测试：

> **同一个 `Resource.Acquire` 语义，无论用户从 C++、C#、Java、Kotlin、Rust 还是 Python 进来，除了语言语法不同之外，对系统的意义、状态变化、错误、权限、Lease/Fence 行为完全一致。**

做到这个程度以后，语言就真的只是“皮肤”，而 Cy Contract 才是平台。
