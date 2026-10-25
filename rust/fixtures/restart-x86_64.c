/* Interrupt a blocked pipe read with and without Linux SA_RESTART. */
static long call(long n,long a,long b,long c,long d) {
    register long r10 __asm__("r10")=d;
    long r;
    __asm__ volatile("syscall":"=a"(r):"a"(n),"D"(a),"S"(b),"d"(c),"r"(r10):"rcx","r11","memory");
    return r;
}
#define CHECK(x) do { if(!(x)) { call(231,__LINE__,0,0,0); __builtin_unreachable(); } } while(0)
static volatile int handled;
static void handler(int signal) { if(signal==10) handled++; }
__attribute__((naked)) static void restore(void) { __asm__ volatile("mov $15,%rax; syscall"); }
__attribute__((noreturn)) void _start(void) {
    for(int restart=0;restart<2;restart++) {
        handled=0;
        struct {void (*handler)(int); unsigned long flags; void (*restore)(void); unsigned long mask;} action={handler,0x4000000ul | (restart ? 0x10000000ul : 0),restore,0};
        CHECK(call(13,10,(long)&action,0,8)==0);
        int pipe[2]; CHECK(call(22,(long)pipe,0,0,0)==0);
        long parent=call(39,0,0,0,0);
        long child=call(57,0,0,0,0); CHECK(child>=0);
        if(child==0) {
            for(volatile int n=0;n<10000;n++) {}
            CHECK(call(62,parent,10,0,0)==0);
            for(volatile int n=0;n<10000;n++) {}
            CHECK(call(1,pipe[1],(long)"x",1,0)==1);
            call(231,0,0,0,0); __builtin_unreachable();
        }
        char value=0;
        long result=call(0,pipe[0],(long)&value,1,0);
        if(!restart) { CHECK(result==-4); result=call(0,pipe[0],(long)&value,1,0); }
        CHECK(result==1 && value=='x' && handled==1);
        int status=-1; CHECK(call(61,child,(long)&status,0,0)==child && status==0);
        CHECK(call(3,pipe[0],0,0,0)==0 && call(3,pipe[1],0,0,0)==0);
    }
    call(231,0,0,0,0); __builtin_unreachable();
}
