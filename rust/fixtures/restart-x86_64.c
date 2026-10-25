/* Interrupt a blocked pipe read with and without Linux SA_RESTART. */
static long call(long n,long a,long b,long c,long d) {
    register long r10 __asm__("r10")=d;
    long r;
    __asm__ volatile("syscall":"=a"(r):"a"(n),"D"(a),"S"(b),"d"(c),"r"(r10):"rcx","r11","memory");
    return r;
}
#define CHECK(x) do { if(!(x)) { call(231,__LINE__,0,0,0); __builtin_unreachable(); } } while(0)
static volatile int handled;
static volatile unsigned handler_mxcsr;
static int acknowledgement, restart_mode;
static void handler(int signal) {
    if(signal==10) {
        __asm__ volatile("stmxcsr %0":"=m"(handler_mxcsr));
        unsigned changed=0x3f80;
        __asm__ volatile("ldmxcsr %0"::"m"(changed));
        handled++;
        if(restart_mode) call(1,acknowledgement,(long)"a",1,0);
    }
}
__attribute__((naked)) static void restore(void) { __asm__ volatile("mov $15,%rax; syscall"); }
__attribute__((noreturn)) void _start(void) {
    for(int restart=1;restart>=0;restart--) {
        handled=0; restart_mode=restart;
        int control[2]; CHECK(call(22,(long)control,0,0,0)==0);
        acknowledgement=control[1];
        struct {void (*handler)(int); unsigned long flags; void (*restore)(void); unsigned long mask;} action={handler,0x4000000ul | (restart ? 0x10000000ul : 0),restore,0};
        CHECK(call(13,10,(long)&action,0,8)==0);
        int pipe[2]; CHECK(call(22,(long)pipe,0,0,0)==0);
        unsigned original=0x5f80;
        __asm__ volatile("ldmxcsr %0"::"m"(original));
        long parent=call(39,0,0,0,0);
        long child=call(57,0,0,0,0); CHECK(child>=0);
        if(child==0) {
            struct {int fd; short events, revents;} ready={control[0],1,0};
            /* Repeat until the parent confirms progress. No host-speed delay can
               guarantee that a single signal lands inside the blocked read. */
            int acknowledged=0;
            for(int attempt=0;attempt<1000;attempt++) {
                CHECK(call(62,parent,10,0,0)==0);
                long polled=call(7,(long)&ready,1,1,0);
                CHECK(polled>=0);
                if(polled==1) { acknowledged=1; break; }
            }
            CHECK(acknowledged);
            CHECK(call(1,pipe[1],(long)"x",1,0)==1);
            call(231,0,0,0,0); __builtin_unreachable();
        }
        char value=0;
        long result=call(0,pipe[0],(long)&value,1,0);
        if(!restart) {
            CHECK(result==-4);
            unsigned long mask=1ul<<9;
            CHECK(call(14,0,(long)&mask,0,8)==0);
            CHECK(call(1,control[1],(long)"a",1,0)==1);
            result=call(0,pipe[0],(long)&value,1,0);
        }
        CHECK(result==1 && value=='x' && handled>=1);
        unsigned restored;
        __asm__ volatile("stmxcsr %0":"=m"(restored));
        CHECK(handler_mxcsr==0x1f80 && restored==original);
        int status=-1; CHECK(call(61,child,(long)&status,0,0)==child && status==0);
        CHECK(call(3,pipe[0],0,0,0)==0 && call(3,pipe[1],0,0,0)==0);
        CHECK(call(3,control[0],0,0,0)==0 && call(3,control[1],0,0,0)==0);
    }
    call(231,0,0,0,0); __builtin_unreachable();
}
