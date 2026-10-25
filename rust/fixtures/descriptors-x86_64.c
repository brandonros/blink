/* Linux descriptor ABI checks, runnable unchanged natively and in both engines. */
static long call(long n,long a,long b,long c,long d) {
    register long r10 __asm__("r10")=d;
    long r;
    __asm__ volatile("syscall":"=a"(r):"a"(n),"D"(a),"S"(b),"d"(c),"r"(r10):"rcx","r11","memory");
    return r;
}
#define CHECK(x) do { if(!(x)) { call(231,__LINE__,0,0,0); __builtin_unreachable(); } } while(0)
struct vector {const char *p; unsigned long n;};
__attribute__((noreturn)) void _start(void) {
    int p[2];
    CHECK(call(293,(long)p,0x80000,0,0)==0);
    CHECK(call(72,p[0],1,0,0)==1);
    CHECK(call(72,p[1],1,0,0)==1);
    CHECK(call(72,p[0],3,0,0)==0);
    CHECK(call(72,p[1],3,0,0)==1);
    long d=call(72,p[1],0,10,0);
    CHECK(d==10 && call(72,d,1,0,0)==0);
    CHECK(call(72,d,2,1,0)==0 && call(72,d,1,0,0)==1);
    CHECK(call(33,d,d,0,0)==d && call(72,d,1,0,0)==1);
    CHECK(call(292,d,11,0x80000,0)==11 && call(72,11,1,0,0)==1);
    struct vector v[]={{"vec",3},{"tors",4}};
    CHECK(call(20,d,(long)v,2,0)==7);
    char b[8]={0};
    CHECK(call(0,p[0],(long)b,8,0)==7);
    CHECK(b[0]=='v' && b[3]=='t' && b[6]=='s');
    CHECK(call(16,d,0x5413,(long)b,0)==-25);
    CHECK(call(3,p[0],0,0,0)==0);
    /* Block SIGPIPE: write and writev must return EPIPE rather than kill us. */
    unsigned long mask=1ul<<12;
    CHECK(call(14,0,(long)&mask,0,8)==0);
    CHECK(call(1,d,(long)"x",1,0)==-32);
    CHECK(call(20,d,(long)v,2,0)==-32);
    CHECK(call(3,d,0,0,0)==0);
    CHECK(call(72,d,1,0,0)==-9);
    call(231,0,0,0,0);
    __builtin_unreachable();
}
