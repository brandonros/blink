/* Child termination notification, siginfo, auto-reap and coalescing. */
static long call(long n,long a,long b,long c,long d) {
    register long r10 __asm__("r10")=d;
    long r;__asm__ volatile("syscall":"=a"(r):"a"(n),"D"(a),"S"(b),"d"(c),"r"(r10):"rcx","r11","memory");return r;
}
#define CHECK(x) do {if(!(x)) {call(231,__LINE__,0,0,0);__builtin_unreachable();}} while(0)
static volatile int seen,lastpid,lastcode,laststatus,lastuid;
static void handler(int signal,void *info,void *context) {
    (void)context;CHECK(signal==17);int *words=info;
    CHECK(words[0]==17 && words[1]==0);
    seen++;lastcode=words[2];lastpid=words[4];lastuid=words[5];laststatus=words[6];
}
__attribute__((naked)) static void restore(void) {__asm__ volatile("mov $15,%rax; syscall");}
__attribute__((noreturn)) void _start(void) {
    struct {void (*handler)(int,void*,void*);unsigned long flags;void (*restore)(void);unsigned long mask;} action={handler,4|0x4000000|0x10000000,restore,0};
    long uid=call(102,0,0,0,0);
    for(int mode=0;mode<4;mode++) {
        seen=0;action.handler=mode==2?(void*)1:handler;
        action.flags=4|0x4000000|0x10000000|(mode==1?2:0);
        CHECK(call(13,17,(long)&action,0,8)==0);
        int pipe[2];CHECK(call(22,(long)pipe,0,0,0)==0);
        long child=call(57,0,0,0,0);CHECK(child>=0);
        if(child==0) {
            char value;CHECK(call(0,pipe[0],(long)&value,1,0)==1);
            call(231,42,0,0,0);__builtin_unreachable();
        }
        if(mode==3) CHECK(call(62,child,9,0,0)==0);
        else CHECK(call(1,pipe[1],(long)"x",1,0)==1);
        int status=-1;long waited=call(61,child,(long)&status,0,0);
        if(mode==1 || mode==2) CHECK(waited==-10);
        else CHECK(waited==child && status==(mode==3?9:42<<8));
        CHECK(call(61,child,0,1,0)==-10);
        if(mode==2) CHECK(seen==0);
        else CHECK(seen==1 && lastpid==child && lastuid==uid && lastcode==(mode==3?2:1) && laststatus==(mode==3?9:42));
        CHECK(call(3,pipe[0],0,0,0)==0 && call(3,pipe[1],0,0,0)==0);
    }
    /* Two standard SIGCHLDs while blocked retain the first event's siginfo. */
    action.flags=4|0x4000000|0x10000000;CHECK(call(13,17,(long)&action,0,8)==0);
    unsigned long mask=1ul<<16;CHECK(call(14,0,(long)&mask,0,8)==0);seen=0;
    long first=0;
    for(int n=0;n<2;n++) {
        long child=call(57,0,0,0,0);CHECK(child>=0);
        if(child==0) {call(231,50+n,0,0,0);__builtin_unreachable();}
        if(n==0) first=child;
        int status;CHECK(call(61,child,(long)&status,0,0)==child && status==((50+n)<<8));
    }
    unsigned long pending=0,empty=0,current=0;
    CHECK(seen==0 && call(127,(long)&pending,8,0,0)==0 && pending==mask);
    CHECK(call(130,(long)&empty,8,0,0)==-4);
    CHECK(seen==1 && lastpid==first && lastcode==1 && laststatus==50);
    CHECK(call(14,0,0,(long)&current,8)==0 && current==mask);
    CHECK(call(127,(long)&pending,8,0,0)==0 && pending==0);
    CHECK(call(14,1,(long)&mask,0,8)==0);
    call(231,0,0,0,0);__builtin_unreachable();
}
