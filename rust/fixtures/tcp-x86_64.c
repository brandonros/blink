/* A host-loopback peer tests Linux guest TCP ABI and cooperative blocking. */
#ifndef PORT
#error PORT must be supplied by the harness
#endif
static long call(long n,long a,long b,long c,long d,long e) {
    register long r10 __asm__("r10")=d;
    register long r8 __asm__("r8")=e;
    register long r9 __asm__("r9")=0;
    long r; __asm__ volatile("syscall":"=a"(r):"a"(n),"D"(a),"S"(b),"d"(c),"r"(r10),"r"(r8),"r"(r9):"rcx","r11","memory");return r;
}
#define CHECK(x) do {if(!(x)) {call(231,__LINE__,0,0,0,0);__builtin_unreachable();}} while(0)
struct vector {const char *p;unsigned long n;};
__attribute__((noreturn)) void _start(void) {
    for(int nonblock=0;nonblock<2;nonblock++) {
        long fd=call(41,2,1|0x80000|(nonblock?0x800:0),6,0,0); CHECK(fd>=0);
        CHECK(call(72,fd,1,0,0,0)==1);
        unsigned char addr[16]={2,0,PORT>>8,PORT&255,127,0,0,1};
        long connected=call(42,fd,(long)addr,sizeof addr,0,0);
        CHECK(connected==0 || (nonblock && connected==-115));
        struct {int fd;short events,revents;} pollfd={fd,4,0};
        CHECK(call(7,(long)&pollfd,1,3000,0,0)==1 && (pollfd.revents&4));
        unsigned error=999,length=4;
        CHECK(call(55,fd,1,4,(long)&error,(long)&length)==0 && error==0 && length==4);
        unsigned yes=1;
        CHECK(call(54,fd,6,1,(long)&yes,4)==0);
        CHECK(call(54,fd,1,9,(long)&yes,4)==0);
        unsigned char peer[16]={0}; length=16;
        CHECK(call(52,fd,(long)peer,(long)&length,0,0)==0 && length==16 && peer[0]==2 && peer[2]==(PORT>>8) && peer[3]==(PORT&255));
        length=16;
        CHECK(call(51,fd,(long)peer,(long)&length,0,0)==0 && length==16 && peer[0]==2 && (peer[2]||peer[3]));
        long duplicate=call(32,fd,0,0,0,0);CHECK(duplicate>=0);
        CHECK(call(72,duplicate,4,0,0,0)==0 && call(72,fd,3,0,0,0)==2);
        CHECK(call(3,duplicate,0,0,0,0)==0);
        struct vector request[]={{"ping:",5},{"request\n",8}};
        CHECK(call(20,fd,(long)request,2,0,0)==13);
        CHECK(call(48,fd,1,0,0,0)==0);
        long child=call(57,0,0,0,0,0);CHECK(child>=0);
        if(child==0) {
            char response[6];long got=0;
            while(got<6) {long n=call(0,fd,(long)(response+got),6-got,0,0);CHECK(n>0);got+=n;}
            CHECK(response[0]=='h' && response[4]=='o' && response[5]=='\n');
            char end;CHECK(call(0,fd,(long)&end,1,0,0)==0);
            CHECK(call(1,1,(long)response,6,0,0)==6);
            CHECK(call(3,fd,0,0,0,0)==0);
            call(231,0,0,0,0,0);__builtin_unreachable();
        }
        CHECK(call(3,fd,0,0,0,0)==0);
        int status=-1;CHECK(call(61,child,(long)&status,0,0,0)==child && status==0);
    }
    call(231,0,0,0,0,0);__builtin_unreachable();
}
