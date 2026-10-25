/* Alternate stacks, nested handlers, autodisarm, fork and exec inheritance. */
static long call(long n,long a,long b,long c,long d) {
    register long r10 __asm__("r10")=d;
    long r;__asm__ volatile("syscall":"=a"(r):"a"(n),"D"(a),"S"(b),"d"(c),"r"(r10):"rcx","r11","memory");return r;
}
#define CHECK(x) do {if(!(x)) {call(231,__LINE__,0,0,0);__builtin_unreachable();}} while(0)
struct stack {unsigned long pointer;unsigned flags,pad;unsigned long size;};
static unsigned char area[32768];
static volatile unsigned seen,autodisarm;
static long self;
__attribute__((naked)) static void restore(void) {__asm__ volatile("mov $15,%rax; syscall");}
static void handler(int signal,void *info,void *context) {
    (void)info;
    unsigned long sp;__asm__ volatile("mov %%rsp,%0":"=r"(sp));
    CHECK(sp>(unsigned long)area && sp<(unsigned long)area+sizeof area);
    struct stack current,disable={0,2,0,0};
    CHECK(call(131,0,(long)&current,0,0)==0);
    struct stack *saved=(struct stack *)((char *)context+16);
    if(autodisarm) {
        CHECK(current.flags==2 && current.pointer==0 && current.size==0);
        CHECK(saved->flags==(signal==10?0x80000000u:2));
    } else {
        CHECK(current.flags==1 && current.pointer==(unsigned long)area && current.size==sizeof area);
        CHECK(saved->pointer==(unsigned long)area && saved->flags==0);
        CHECK(call(131,(long)&disable,0,0,0)==-1);
    }
    if(signal==10) {seen|=1;CHECK(call(62,self,12,0,0)==0);CHECK(seen==3);}
    else {CHECK(signal==12);seen|=2;}
}
__attribute__((used,noinline,noreturn)) static void run(long *initial) {
    struct stack old,stack={(unsigned long)area,0,0,sizeof area},disable={0,2,0,0};
    CHECK(call(131,0,(long)&old,0,0)==0 && old.flags==(initial[0]>1?0x80000002u:2));
    if(initial[0]>1) {call(231,0,0,0,0);__builtin_unreachable();}
    stack.size=2047;CHECK(call(131,(long)&stack,0,0,0)==-12);
    stack.size=sizeof area;stack.flags=4;CHECK(call(131,(long)&stack,0,0,0)==-22);stack.flags=0;
    CHECK(call(131,1,0,0,0)==-14);
    CHECK(call(131,(long)&stack,(long)&old,0,0)==0 && old.flags==2);
    self=call(39,0,0,0,0);
    struct {void (*handler)(int,void*,void*);unsigned long flags;void (*restore)(void);unsigned long mask;} action={handler,0xffffffff00000000ul|4|0x4000000|0x8000000,restore,0};
    CHECK(call(13,10,(long)&action,0,8)==0 && call(13,12,(long)&action,0,8)==0);
    CHECK(call(62,self,10,0,0)==0 && seen==3);
    CHECK(call(131,0,(long)&old,0,0)==0 && old.flags==0 && old.pointer==stack.pointer);
    stack.flags=0x80000000u;autodisarm=1;seen=0;
    CHECK(call(131,(long)&stack,0,0,0)==0);
    CHECK(call(62,self,10,0,0)==0 && seen==3);
    CHECK(call(131,0,(long)&old,0,0)==0 && old.flags==0x80000000u && old.pointer==stack.pointer);
    long child=call(57,0,0,0,0);CHECK(child>=0);
    if(child==0) {
        CHECK(call(131,0,(long)&old,0,0)==0 && old.flags==stack.flags && old.pointer==stack.pointer);
        char *argv[]={(char *)initial[1],"exec",0},*env[]={0};
        call(59,(long)argv[0],(long)argv,(long)env,0);
        call(231,99,0,0,0);__builtin_unreachable();
    }
    int status=-1;CHECK(call(61,child,(long)&status,0,0)==child && status==0);
    CHECK(call(131,(long)&disable,(long)&old,0,0)==0 && old.flags==stack.flags);
    CHECK(call(131,0,(long)&old,0,0)==0 && old.flags==2 && old.size==0 && old.pointer==0);
    call(231,0,0,0,0);__builtin_unreachable();
}
__attribute__((naked,noreturn)) void _start(void) {__asm__ volatile("mov %rsp,%rdi; and $-16,%rsp; call run; ud2");}
