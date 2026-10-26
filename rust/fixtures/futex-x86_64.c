/* Requeue actual guest waiters, then compare/requeue one wake and two transfers. */
static long call(long n,long a,long b,long c,long d,long e,long f) {register long r10 __asm__("r10")=d;register long r8 __asm__("r8")=e;register long r9 __asm__("r9")=f;long r;__asm__ volatile("syscall":"=a"(r):"a"(n),"D"(a),"S"(b),"d"(c),"r"(r10),"r"(r8),"r"(r9):"rcx","r11","memory");return r;}
#define CHECK(x) do {if(!(x)){call(231,__LINE__,0,0,0,0,0);__builtin_unreachable();}} while(0)
static volatile unsigned source,target,ready,done;
static unsigned char stacks[3][8192] __attribute__((aligned(16)));
__attribute__((used,noinline)) static void worker(void) {
    __atomic_add_fetch(&ready,1,__ATOMIC_SEQ_CST);
    CHECK(call(202,(long)&source,128,0,0,0,0)==0);
    __atomic_add_fetch(&done,1,__ATOMIC_SEQ_CST);
}
__attribute__((naked)) static long spawn(void *stack) {
    (void)stack;__asm__ volatile("mov %rdi,%rsi; mov $0x10f00,%edi; mov $56,%eax; syscall; test %eax,%eax; jnz 1f; call worker; xor %edi,%edi; mov $60,%eax; syscall; 1: ret");
}
__attribute__((noreturn)) void _start(void) {
    CHECK(call(202,(long)&source,131,-1,1,(long)&target,0)==-22);
    CHECK(call(202,(long)&source,131,0,-1,(long)&target,0)==-22);
    CHECK(call(202,(long)&source,131,0,1,(long)&target+1,0)==-22);
    for(int n=0;n<3;n++)CHECK(spawn(stacks[n]+sizeof(stacks[n]))>0);
    while(ready!=3)__asm__ volatile("pause");
    long moved=0;
    while(moved<3) {long result=call(202,(long)&source,131,0,3-moved,(long)&target,0);CHECK(result>=0);moved+=result;}
    CHECK(done==0 && call(202,(long)&source,129,3,0,0,0)==0);
    CHECK(call(202,(long)&target,132,1,2,(long)&source,1)==-11 && done==0);
    CHECK(call(202,(long)&target,132,1,2,(long)&source,0)==3);
    CHECK(call(202,(long)&target,129,3,0,0,0)==0);
    CHECK(call(202,(long)&source,129,3,0,0,0)==2);
    while(done!=3)__asm__ volatile("pause");
    CHECK(source==0 && target==0);
    call(231,0,0,0,0,0,0);__builtin_unreachable();
}
